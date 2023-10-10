use std::collections::BTreeSet;
use std::fmt::{Debug, Formatter};
use std::io::{Cursor, Read};
use std::sync::Arc;

use ble_peripheral::advertisement::{AdType, AdvertisementRequest, ConnectMode};
use ble_peripheral::gap_advertiser::GapAdvertiser;
use ble_peripheral::prelude::{AdvertisementParams, AdvertisementPayload, DiscoverMode};
use bluer::adv::Feature;
use bluer::{Uuid, UuidExt};
use byteorder::{LittleEndian, ReadBytesExt};
use log::warn;
use tokio::sync::mpsc;

use crate::device_ident::DeviceIdent;
use crate::err_util::generic_err;
use crate::event::Event;

const DISCOVER_MODE_MASK: u8 = 0b0000_0011;

#[derive(Clone)]
pub struct BluerAdvertiser {
  pub tx: mpsc::UnboundedSender<Event>,
  pub service_uuids: Arc<BTreeSet<Uuid>>,
  pub ident: Arc<DeviceIdent>,
}

impl Debug for BluerAdvertiser {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BluerAdvertiser")
      .field("service_uuids", &self.service_uuids)
      .field("ident", &self.ident)
      .finish_non_exhaustive()
  }
}

impl GapAdvertiser for BluerAdvertiser {
  fn request_start(&self, request: AdvertisementRequest) {
    let event = match AdvertisementFactory::convert(request) {
      Ok(adv) => Event::RequestAdvStart(adv),
      Err(e) => Event::OnAdvStartResult(Err(e)),
    };
    let _ = self.tx.send(event);
  }

  fn request_stop(&self) {
    let _ = self.tx.send(Event::RequestAdvStop);
  }
}

struct AdvertisementFactory {
  output: bluer::adv::Advertisement,
}

impl AdvertisementFactory {
  pub fn convert(request: AdvertisementRequest) -> Result<bluer::adv::Advertisement, bluer::Error> {
    if request.scan_response_payload.is_some() {
      return Err(generic_err("Scan response payload is not supported!"));
    }

    let mut me = Self {
      output: Default::default(),
    };
    me.process_params(request.params)?;
    me.process_payload(request.payload)?;
    Ok(me.output)
  }

  fn process_params(&mut self, params: AdvertisementParams) -> Result<(), bluer::Error> {
    if params.connect_mode != ConnectMode::Directed {
      return Err(generic_err(&format!(
        "Unsupported connect_mode={:?}",
        params.connect_mode
      )));
    }
    self.output.min_interval = params.interval_min;
    self.output.max_interval = params.interval_max;
    Ok(())
  }

  fn process_payload(&mut self, payload: AdvertisementPayload) -> Result<(), bluer::Error> {
    let iter = AdvertisementIter::new(&payload);
    for next in iter {
      self.process_ad_record(next)?;
    }
    Ok(())
  }

  fn process_ad_record(&mut self, record: AdvertisementRecord) -> Result<(), bluer::Error> {
    match record.ad_type {
      t if t == AdType::Flags as _ => {
        let flags = record
          .data
          .first()
          .ok_or_else(|| generic_err("Flags missing data!"))?;
        let is_discoverable = flags & DISCOVER_MODE_MASK == DiscoverMode::General as _;
        self.output.discoverable = Some(is_discoverable);
      }
      t if t == AdType::ShortLocalName as _ || t == AdType::LongLocalName as _ => {
        let name = String::from_utf8(record.data.to_vec())
          .map_err(|e| generic_err(&format!("invalid local name: {e}")))?;
        self.output.local_name.get_or_insert(name);
        self.output.system_includes.insert(Feature::LocalName);
      }
      t if t == AdType::Appearance as _ => {
        let appearance = Cursor::new(record.data)
          .read_u16::<LittleEndian>()
          .map_err(|e| generic_err(&format!("invalid appearance: {e}")))?;
        self.output.appearance = Some(appearance);
        self.output.system_includes.insert(Feature::Appearance);
      }
      t if t == AdType::ManufacturerData as _ => {
        let mut cursor = Cursor::new(record.data);
        let manufacturer_id = cursor
          .read_u16::<LittleEndian>()
          .map_err(|e| generic_err(&format!("invalid manufacturer id: {e}")))?;
        self
          .output
          .manufacturer_data
          .insert(manufacturer_id, read_remainder(cursor));
      }
      t if t == AdType::ServiceData16 as _ => {
        let mut cursor = Cursor::new(record.data);
        let service_uuid = cursor
          .read_u16::<LittleEndian>()
          .map_err(|e| generic_err(&format!("invalid service id: {e}")))?;
        self
          .output
          .service_data
          .insert(Uuid::from_u16(service_uuid), read_remainder(cursor));
      }
      t if t == AdType::CompleteServiceUuids16 as _ => {
        let mut cursor = Cursor::new(record.data);
        while let Ok(uuid) = cursor.read_u16::<LittleEndian>() {
          self.output.service_uuids.insert(Uuid::from_u16(uuid));
        }
      }
      t if t == AdType::CompleteServiceUuids128 as _ => {
        let mut cursor = Cursor::new(record.data);
        while let Ok(uuid) = cursor.read_u128::<LittleEndian>() {
          self.output.service_uuids.insert(Uuid::from_u128(uuid));
        }
      }
      t => {
        warn!("Unsupported ad_type={t}");
      }
    }
    Ok(())
  }
}

fn read_remainder<T: AsRef<[u8]>>(mut cursor: Cursor<T>) -> Vec<u8> {
  let mut out = Vec::new();
  cursor.read_to_end(&mut out).unwrap();
  out
}

struct AdvertisementIter<'a> {
  cursor: Cursor<&'a [u8]>,
}

impl<'a> AdvertisementIter<'a> {
  pub fn new(data: &'a [u8]) -> Self {
    Self {
      cursor: Cursor::new(data),
    }
  }
}

impl<'a> Iterator for AdvertisementIter<'a> {
  type Item = AdvertisementRecord;

  fn next(&mut self) -> Option<Self::Item> {
    let length = self.cursor.read_u8().ok()?;
    if length < 1 {
      return None;
    }
    let ad_type = self.cursor.read_u8().ok()?;
    let mut data = vec![0u8; (length - 1).into()];
    self.cursor.read_exact(&mut data).ok()?;

    Some(AdvertisementRecord { ad_type, data })
  }
}

struct AdvertisementRecord {
  ad_type: u8,
  data: Vec<u8>,
}

#[cfg(test)]
mod tests {
  use crate::advertiser::AdvertisementFactory;
  use ble_peripheral::advertisement::AdvertisementPayloadBuilder;
  use ble_peripheral::prelude::{AdvertisementParams, AdvertisementRequest, DiscoverMode};

  #[test]
  fn test_flags() {
    let payload = AdvertisementPayloadBuilder::new()
      .set_discover_mode(DiscoverMode::None)
      .build()
      .unwrap();
    let req = AdvertisementRequest {
      params: AdvertisementParams::default(),
      payload,
      scan_response_payload: None,
    };

    let converted = AdvertisementFactory::convert(req).unwrap();

    assert_eq!(converted.discoverable, Some(false));
  }

  #[test]
  fn test_manufacturer_data() {
    let mfg_id = 0x1234;
    let data = [0x1, 0x2, 0x3, 0x4];
    let payload = AdvertisementPayloadBuilder::new()
      .push_manufacturer_data(mfg_id, &data)
      .unwrap()
      .build()
      .unwrap();
    let req = AdvertisementRequest {
      params: AdvertisementParams::default(),
      payload,
      scan_response_payload: None,
    };

    let converted = AdvertisementFactory::convert(req).unwrap();

    let actual: Vec<_> = converted.manufacturer_data.into_iter().collect();
    assert_eq!(actual, vec![(mfg_id, data.to_vec())]);
  }
}
