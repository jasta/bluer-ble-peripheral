use bluer::gatt::local::ApplicationHandle;
use ble_peripheral::descriptors::{AttributeHandle, UUID};
use bluer::adv::AdvertisementHandle;
use ble_peripheral::gatt_server_cb::WriteAction;
use crate::advertiser::BluerAdvertiser;
use crate::connection::BluerConnection;
use crate::responder::BluerResponder;
use crate::writer::BluerWriter;

pub enum Event {
    OnStartResult(
        Result<
            (
                ApplicationHandle,
                BluerAdvertiser,
                Vec<(UUID, AttributeHandle)>,
            ),
            bluer::Error,
        >,
    ),
    OnHandleDrop,
    RequestAdvStart(bluer::adv::Advertisement),
    RequestAdvStop,
    OnAdvStartResult(Result<AdvertisementHandle, bluer::Error>),
    OnHandleRead {
        conn: BluerConnection,
        mtu: Option<u16>,
        handle: AttributeHandle,
        responder: BluerResponder,
    },
    OnHandleWrite {
        conn: BluerConnection,
        mtu: Option<u16>,
        handle: AttributeHandle,
        responder: Option<BluerResponder>,
        action: WriteAction,
        offset: u16,
        value: Vec<u8>,
    },
    OnSubscribe {
        conn: BluerConnection,
        handle: AttributeHandle,
        writer: BluerWriter,
    },
    OnUnsubscribe {
        conn: BluerConnection,
        handle: AttributeHandle,
    },
}
