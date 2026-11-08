//! `FlashStore` against an emulated NOR flash, where writes can only clear
//! bits and erases set whole sectors to `0xFF`.

use embassy_futures::block_on;
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use impromptu_wifi_core::traits::{CredentialStore, Credentials};
use impromptu_wifi_embassy::flash::{FlashStore, FlashStoreError};

const SECTOR: usize = 4096;

struct Nor {
    bytes: Vec<u8>,
    erases: u32,
}

impl Nor {
    fn new(sectors: usize) -> Self {
        Self {
            bytes: vec![0xFF; sectors * SECTOR],
            erases: 0,
        }
    }
}

#[derive(Debug, PartialEq)]
struct OutOfBounds;

impl NorFlashError for OutOfBounds {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::OutOfBounds
    }
}

impl ErrorType for Nor {
    type Error = OutOfBounds;
}

impl ReadNorFlash for Nor {
    const READ_SIZE: usize = 4;
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), OutOfBounds> {
        let at = offset as usize;
        let src = self.bytes.get(at..at + bytes.len()).ok_or(OutOfBounds)?;
        bytes.copy_from_slice(src);
        Ok(())
    }
    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for Nor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR;
    fn erase(&mut self, from: u32, to: u32) -> Result<(), OutOfBounds> {
        self.erases += 1;
        let range = self
            .bytes
            .get_mut(from as usize..to as usize)
            .ok_or(OutOfBounds)?;
        range.fill(0xFF);
        Ok(())
    }
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), OutOfBounds> {
        let at = offset as usize;
        let dst = self
            .bytes
            .get_mut(at..at + bytes.len())
            .ok_or(OutOfBounds)?;
        for (d, s) in dst.iter_mut().zip(bytes) {
            *d &= s;
        }
        Ok(())
    }
}

#[test]
fn saves_loads_and_clears() {
    let mut store = FlashStore::new(Nor::new(4), SECTOR as u32).unwrap();
    assert_eq!(
        block_on(store.load()),
        Ok(None),
        "erased flash holds nothing"
    );

    let home = Credentials::new("home", "hunter22").unwrap();
    block_on(store.save(&home)).unwrap();
    assert_eq!(block_on(store.load()), Ok(Some(home)));

    // A second save replaces the first, so it must erase before writing.
    let open = Credentials::new("café", "").unwrap();
    block_on(store.save(&open)).unwrap();
    assert_eq!(block_on(store.load()), Ok(Some(open)));

    block_on(store.clear()).unwrap();
    assert_eq!(block_on(store.load()), Ok(None));

    let flash = store.into_inner();
    assert!(
        flash.bytes[..SECTOR].iter().all(|&b| b == 0xFF),
        "other sectors untouched"
    );
    assert!(flash.bytes[2 * SECTOR..].iter().all(|&b| b == 0xFF));
}

#[test]
fn foreign_or_damaged_data_loads_as_nothing() {
    let mut nor = Nor::new(1);
    nor.bytes[..8].copy_from_slice(b"garbage!");
    let mut store = FlashStore::new(nor, 0).unwrap();
    assert_eq!(block_on(store.load()), Ok(None));

    let longest = Credentials::new(&"s".repeat(32), &"p".repeat(64)).unwrap();
    block_on(store.save(&longest)).unwrap();
    assert_eq!(block_on(store.load()), Ok(Some(longest)));
    let mut nor = store.into_inner();
    nor.bytes[10] = 0; // a torn write
    let mut store = FlashStore::new(nor, 0).unwrap();
    assert_eq!(block_on(store.load()), Ok(None));
}

#[test]
fn the_region_must_be_a_whole_sector_inside_the_flash() {
    assert!(matches!(
        FlashStore::new(Nor::new(2), 100),
        Err(FlashStoreError::Region)
    ));
    assert!(matches!(
        FlashStore::new(Nor::new(2), 2 * SECTOR as u32),
        Err(FlashStoreError::Region)
    ));
    assert!(FlashStore::new(Nor::new(2), SECTOR as u32).is_ok());
}
