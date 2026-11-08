//! A Web Bluetooth connection as a `GattClient`.

use std::rc::Rc;

use impromptu_wifi_ble::{GattClient, Notification};
use impromptu_wifi_client::proto::ble::{
    CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_COMMAND_UUID, RPC_RESULT_UUID,
    SERVICE_UUID,
};
use js_sys::{DataView, JsString, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    BluetoothDevice, BluetoothLeScanFilterInit, BluetoothRemoteGattCharacteristic,
    BluetoothRemoteGattServer, RequestDeviceOptions,
};

use crate::queue::{Event, NotificationQueue};
use crate::uuid::uuid_string;
use crate::WebError;

type Listener = Closure<dyn FnMut(web_sys::Event)>;

/// A connected Improv device over Web Bluetooth.
///
/// Notifications are collected by event listeners into a queue, so
/// [`GattClient::notification`] is cancel-safe.
pub struct WebGatt {
    device: BluetoothDevice,
    server: BluetoothRemoteGattServer,
    state: BluetoothRemoteGattCharacteristic,
    rpc_command: BluetoothRemoteGattCharacteristic,
    rpc_result: BluetoothRemoteGattCharacteristic,
    capabilities: BluetoothRemoteGattCharacteristic,
    queue: Rc<NotificationQueue>,
    // Registered listeners, removed again on drop.
    listeners: Vec<(web_sys::EventTarget, &'static str, Listener)>,
}

impl WebGatt {
    /// Ask the user to choose a device advertising the Improv service,
    /// connect, and subscribe to its notifications.
    pub async fn request() -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or("no window")?;
        let bluetooth = window
            .navigator()
            .bluetooth()
            .ok_or("this browser has no Web Bluetooth")?;
        let filter = BluetoothLeScanFilterInit::new();
        filter.set_services(&[JsString::from(uuid_string(SERVICE_UUID))]);
        let options = RequestDeviceOptions::new();
        options.set_filters(&[filter]);
        let device: BluetoothDevice = bluetooth.request_device(&options).await?;
        let server = device.gatt().ok_or("the device has no GATT server")?;
        let server: BluetoothRemoteGattServer = server.connect().await?;
        let service = server
            .get_primary_service_with_str(&uuid_string(SERVICE_UUID))
            .await?;
        let characteristic = |uuid: u128| service.get_characteristic_with_str(&uuid_string(uuid));
        let state = characteristic(CURRENT_STATE_UUID).await?;
        let error = characteristic(ERROR_STATE_UUID).await?;
        let rpc_command = characteristic(RPC_COMMAND_UUID).await?;
        let rpc_result = characteristic(RPC_RESULT_UUID).await?;
        let capabilities = characteristic(CAPABILITIES_UUID).await?;

        let queue = Rc::new(NotificationQueue::default());
        let mut gatt = Self {
            device: device.clone(),
            server,
            state: state.clone(),
            rpc_command,
            rpc_result: rpc_result.clone(),
            capabilities,
            queue: queue.clone(),
            listeners: Vec::new(),
        };
        gatt.listen(&state, Event::State);
        gatt.listen(&error, Event::Error);
        gatt.listen(&rpc_result, |_| Event::RpcResult);
        let q = queue.clone();
        gatt.add(
            device.unchecked_ref(),
            "gattserverdisconnected",
            Closure::new(move |_: web_sys::Event| q.push(Event::Disconnected)),
        );
        for c in [&state, &error, &rpc_result] {
            c.start_notifications().await?;
        }
        Ok(gatt)
    }

    /// The device's advertised name.
    pub fn name(&self) -> Option<String> {
        self.device.name()
    }

    /// Close the connection.
    pub fn disconnect(&self) {
        self.server.disconnect();
    }

    fn listen(&mut self, c: &BluetoothRemoteGattCharacteristic, event: fn(u8) -> Event) {
        let q = self.queue.clone();
        let listener = Closure::new(move |e: web_sys::Event| {
            let first = e
                .target()
                .and_then(|t| t.dyn_into::<BluetoothRemoteGattCharacteristic>().ok())
                .and_then(|c| c.value())
                .filter(|v| v.byte_length() > 0)
                .map_or(0, |v| v.get_uint8(0));
            q.push(event(first));
        });
        self.add(c.unchecked_ref(), "characteristicvaluechanged", listener);
    }

    fn add(&mut self, target: &web_sys::EventTarget, name: &'static str, listener: Listener) {
        let _ = target.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        self.listeners.push((target.clone(), name, listener));
    }
}

impl Drop for WebGatt {
    fn drop(&mut self) {
        for (target, name, listener) in &self.listeners {
            let _ =
                target.remove_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        }
        if self.server.connected() {
            self.server.disconnect();
        }
    }
}

fn gatt_error(e: JsValue) -> WebError {
    if is_network_error(&e) {
        WebError::Disconnected
    } else {
        WebError::Js(format!("{e:?}"))
    }
}

/// Chrome rejects GATT operations on a dropped link with a `NetworkError`.
fn is_network_error(e: &JsValue) -> bool {
    js_sys::Reflect::get(e, &"name".into())
        .ok()
        .and_then(|n| n.as_string())
        .is_some_and(|n| n == "NetworkError")
}

fn first_byte(v: &DataView) -> u8 {
    if v.byte_length() > 0 {
        v.get_uint8(0)
    } else {
        0
    }
}

impl GattClient for WebGatt {
    type Error = WebError;

    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), WebError> {
        let data = Uint8Array::from(chunk);
        self.rpc_command
            .write_value_with_response_with_u8_array(&data)
            .map_err(gatt_error)?
            .await
            .map_err(gatt_error)?;
        Ok(())
    }

    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, WebError> {
        match self.queue.next().await {
            Event::State(v) => Ok(Notification::State(v)),
            Event::Error(v) => Ok(Notification::Error(v)),
            Event::Disconnected => Err(WebError::Disconnected),
            Event::RpcResult => {
                // A notification can be cut to the MTU, which Web Bluetooth
                // doesn't expose; a read returns the whole value.
                let value: DataView = self.rpc_result.read_value().await.map_err(gatt_error)?;
                let bytes = Uint8Array::new_with_byte_offset_and_length(
                    &value.buffer(),
                    value.byte_offset() as u32,
                    value.byte_length() as u32,
                )
                .to_vec();
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                Ok(Notification::RpcResult(bytes.len()))
            }
        }
    }

    async fn read_state(&mut self) -> Result<u8, WebError> {
        let v: DataView = self.state.read_value().await.map_err(gatt_error)?;
        Ok(first_byte(&v))
    }

    async fn read_capabilities(&mut self) -> Result<u8, WebError> {
        let v: DataView = self.capabilities.read_value().await.map_err(gatt_error)?;
        Ok(first_byte(&v))
    }
}
