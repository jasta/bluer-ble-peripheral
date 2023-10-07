use crate::device_ident::DeviceIdent;
use crate::event::Event;
use ble_peripheral::gap_advertiser::{Advertisement, GapAdvertiser};
use bluer::adv::Type;
use bluer::Uuid;
use std::collections::BTreeSet;
use std::fmt::{Debug, Formatter};
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::mpsc;

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
  fn request_start(&self, advertisement: Advertisement) {
    let adv = bluer::adv::Advertisement {
      advertisement_type: Type::Peripheral,
      service_uuids: self.service_uuids.deref().clone(),
      manufacturer_data: advertisement.manufacturer_data,
      discoverable: Some(advertisement.is_discoverable),
      local_name: self.ident.name.clone(),
      ..Default::default()
    };

    let _ = self.tx.send(Event::RequestAdvStart(adv));
  }

  fn request_stop(&self) {
    let _ = self.tx.send(Event::RequestAdvStop);
  }
}
