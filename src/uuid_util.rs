use ble_peripheral::descriptors::UUID;
use bluer::Uuid;

pub fn convert_uuid(trait_uuid: UUID) -> Uuid {
    Uuid::from_u128(trait_uuid.as_u128())
}
