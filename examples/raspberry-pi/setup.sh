#!/bin/sh
# Install impromptu-wifi-device on a Raspberry Pi and make it provisionable
# over Improv Wi-Fi. Run on the Pi as root, from a directory that holds the
# binary and this repository's dist files (see README.md):
#
#   sudo ./setup.sh --country SE --ble
#   sudo ./setup.sh --country SE --ble --serial /dev/ttyGS0
#   sudo ./setup.sh --country SE --ble --backend wpa-supplicant
#
# Safe to run again: every step checks before it changes anything. With the
# default backend, NetworkManager, run it over Ethernet, USB networking or a
# local console: on Raspberry Pi OS 11 and older it switches the Pi from
# dhcpcd to NetworkManager, which drops Wi-Fi configured through
# wpa_supplicant.conf. `--backend wpa-supplicant` keeps dhcpcd and
# wpa_supplicant as they are.
set -eu

COUNTRY=""
TRANSPORT=""
BACKEND=networkmanager
OPTIONS=""
BIN=./impromptu-wifi-device
DIST=./dist

usage() {
    echo "usage: $0 --country CC [--ble] [--serial PORT] [--backend networkmanager|wpa-supplicant] [--ignore-wired] [--bin PATH] [--dist DIR]" >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --country) COUNTRY=${2:?}; shift 2 ;;
        --ble) TRANSPORT="$TRANSPORT --ble"; shift ;;
        --serial) TRANSPORT="$TRANSPORT --serial ${2:?}"; shift 2 ;;
        --backend)
            case "${2:?}" in networkmanager|wpa-supplicant) BACKEND=$2 ;; *) usage ;; esac
            shift 2 ;;
        --ignore-wired) OPTIONS="$OPTIONS --ignore-wired"; shift ;;
        --bin) BIN=${2:?}; shift 2 ;;
        --dist) DIST=${2:?}; shift 2 ;;
        *) usage ;;
    esac
done
[ -n "$TRANSPORT" ] || usage
[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
[ -x "$BIN" ] || { echo "no executable at $BIN" >&2; exit 1; }

say() { printf '==> %s\n' "$*"; }

# --- Wi-Fi country -----------------------------------------------------------
# Raspberry Pi OS keeps Wi-Fi blocked until a country is set.
current=$(raspi-config nonint get_wifi_country 2>/dev/null || true)
if [ -z "$current" ]; then
    [ -n "$COUNTRY" ] || { echo "no Wi-Fi country set; pass --country CC" >&2; exit 1; }
    say "setting the Wi-Fi country to $COUNTRY"
    raspi-config nonint do_wifi_country "$COUNTRY"
fi

# --- Wi-Fi backend -------------------------------------------------------------
apt_install() {
    dpkg -s "$1" >/dev/null 2>&1 || { say "installing $1"; apt-get install -y "$1"; }
}

if [ "$BACKEND" = networkmanager ]; then
    apt_install network-manager
    if systemctl is-enabled dhcpcd >/dev/null 2>&1; then
        # Raspberry Pi OS 11 (Bullseye) and older run dhcpcd. Switch to
        # NetworkManager the supported way. Both can't run: Debian's
        # NetworkManager.service declares Conflicts=dhcpcd.service, so at the
        # next boot dhcpcd wouldn't start at all.
        say "switching from dhcpcd to NetworkManager"
        echo "    Wi-Fi set up through wpa_supplicant.conf is not carried over;" >&2
        echo "    a session over that Wi-Fi drops now. Ethernet comes back by DHCP." >&2
        raspi-config nonint do_netconf 2
    fi
    if ! systemctl is-active NetworkManager >/dev/null 2>&1; then
        say "enabling NetworkManager"
        systemctl enable --now NetworkManager
    fi
    # Fresh Raspberry Pi OS 13 images start with NetworkManager's Wi-Fi
    # radio switched off; nothing could be joined.
    if [ "$(nmcli radio wifi)" = disabled ]; then
        say "switching NetworkManager's Wi-Fi radio on"
        nmcli radio wifi on
    fi
else
    # dhcpcd runs wpa_supplicant for wlan0 with /etc/wpa_supplicant/
    # wpa_supplicant.conf. The daemon needs its control socket, open to the
    # group named there, and update_config=1 to save networks.
    conf=/etc/wpa_supplicant/wpa_supplicant.conf
    if systemctl is-active NetworkManager >/dev/null 2>&1; then
        echo "NetworkManager is running; use --backend networkmanager" >&2
        exit 1
    fi
    grep -q '^ctrl_interface=.*GROUP=netdev' "$conf" ||
        echo "warning: $conf has no ctrl_interface=... GROUP=netdev; the service can't reach wpa_supplicant" >&2
    grep -q '^update_config=1' "$conf" ||
        echo "warning: $conf lacks update_config=1; joined networks won't be saved" >&2
    OPTIONS="$OPTIONS --backend wpa-supplicant"
fi

# --- BlueZ -------------------------------------------------------------------
case "$TRANSPORT" in *--ble*)
    # Fresh Raspberry Pi OS images can start with Bluetooth soft-blocked;
    # BlueZ then can't power the adapter. systemd-rfkill keeps the change
    # across reboots.
    for r in /sys/class/rfkill/rfkill*; do
        [ -e "$r/type" ] || continue
        if [ "$(cat "$r/type")" = bluetooth ] && [ "$(cat "$r/soft")" = 1 ]; then
            say "unblocking Bluetooth (rfkill)"
            echo 0 >"$r/soft"
        fi
    done
    dropin=/etc/systemd/system/bluetooth.service.d/impromptu-wifi.conf
    bluetoothd=$(systemctl show -p ExecStart --value bluetooth | sed -n 's/.*path=\([^ ;]*\).*/\1/p')
    if [ ! -f "$dropin" ]; then
        say "starting bluetoothd without its MIDI plugin"
        mkdir -p "$(dirname "$dropin")"
        cat >"$dropin" <<EOF
# BlueZ's MIDI plugin reads a connecting client's BLE-MIDI service, which
# needs encryption; when the client refuses to pair, BlueZ drops the link.
# PipeWire offers BLE-MIDI on Linux desktops, so their provisioning sessions
# were cut after a few seconds.
[Service]
ExecStart=
ExecStart=${bluetoothd:-/usr/libexec/bluetooth/bluetoothd} --noplugin=midi
EOF
        restart_bluetooth=1
    fi
    # BlueZ 5.55 and 5.66 ignore an advertisement's own interval and use
    # these defaults (units of 0.625 ms); at 1.28 s, scans missed the Pi.
    # The keys are LE...AdvertisementInterval in 5.55, ...AdvertisementInterval
    # in the [LE] section in 5.66 and later. 5.82 uses the daemon's interval.
    if grep -qE '^#(LE)?MinAdvertisementInterval=' /etc/bluetooth/main.conf; then
        say "advertising every 100 to 150 ms"
        cp -n /etc/bluetooth/main.conf /etc/bluetooth/main.conf.orig
        sed -i -E 's/^#(LE)?MinAdvertisementInterval=.*/\1MinAdvertisementInterval=160/; s/^#(LE)?MaxAdvertisementInterval=.*/\1MaxAdvertisementInterval=240/' /etc/bluetooth/main.conf
        restart_bluetooth=1
    fi
    # Desktop images run PipeWire, whose WirePlumber offers a BLE-MIDI
    # service and reads a client's. Both need encryption, so every
    # provisioning session from a Linux desktop (which has the same) turned
    # into a pairing attempt: refused, the link dropped; and one laptop's
    # BlueZ 5.87 crashed on the failure.
    if command -v wireplumber >/dev/null 2>&1; then
        if wireplumber --version 2>/dev/null | grep -q 'libwireplumber 0\.5'; then
            conf=/etc/wireplumber/wireplumber.conf.d/90-impromptu-wifi-no-ble-midi.conf
            if [ ! -f "$conf" ]; then
                say "turning off WirePlumber's BLE-MIDI"
                mkdir -p "$(dirname "$conf")"
                printf '%s\n' '# impromptu-wifi-device: no BLE-MIDI on a provisioning device.' \
                    'wireplumber.profiles = {' '  main = {' '    monitor.bluez-midi = disabled' '  }' '}' >"$conf"
                restart_wireplumber=1
            fi
        else
            conf=/etc/wireplumber/bluetooth.lua.d/90-impromptu-wifi-no-ble-midi.lua
            if [ ! -f "$conf" ]; then
                say "turning off WirePlumber's BLE-MIDI"
                mkdir -p "$(dirname "$conf")"
                printf '%s\n' '-- impromptu-wifi-device: no BLE-MIDI on a provisioning device.' \
                    'bluez_midi_monitor.enabled = false' >"$conf"
                restart_wireplumber=1
            fi
        fi
        if [ -n "${restart_wireplumber:-}" ]; then
            for uid in $(ps -C wireplumber -o uid= | sort -u); do
                systemctl --user -M "$(id -nu "$uid")@" restart wireplumber || true
            done
        fi
    fi
    if [ -n "${restart_bluetooth:-}" ]; then
        systemctl daemon-reload
        systemctl restart bluetooth
    fi
;; esac

# --- The service -------------------------------------------------------------
say "installing impromptu-wifi-device"
install -m 755 "$BIN" /usr/local/bin/impromptu-wifi-device
id impromptu-wifi >/dev/null 2>&1 ||
    useradd --system --no-create-home --groups dialout,bluetooth impromptu-wifi
# netdev: wpa_supplicant's control socket (wpa-supplicant backend).
[ "$BACKEND" = networkmanager ] || usermod -aG netdev impromptu-wifi

# polkit 0.106 and later read JavaScript rules; 0.105 (Debian 11 and
# older) only .pkla files.
polkit=$(pkaction --version 2>/dev/null | sed -n 's/.* \([0-9.]*\)$/\1/p')
case "$polkit" in
    0.105|0.10[0-4]*)
        mkdir -p /etc/polkit-1/localauthority/50-local.d
        install -m 644 "$DIST/impromptu-wifi-device.pkla" /etc/polkit-1/localauthority/50-local.d/ ;;
    *)
        install -m 644 "$DIST/50-impromptu-wifi-device.rules" /etc/polkit-1/rules.d/ ;;
esac

install -m 644 "$DIST/impromptu-wifi-device.service" /etc/systemd/system/
mkdir -p /etc/systemd/system/impromptu-wifi-device.service.d
cat >/etc/systemd/system/impromptu-wifi-device.service.d/transport.conf <<EOF
[Service]
ExecStart=
ExecStart=/usr/local/bin/impromptu-wifi-device$TRANSPORT$OPTIONS
EOF
systemctl daemon-reload
systemctl enable impromptu-wifi-device >/dev/null 2>&1
systemctl restart impromptu-wifi-device
sleep 3
systemctl --no-pager --lines=3 status impromptu-wifi-device | sed -n '1,3p;$p'
