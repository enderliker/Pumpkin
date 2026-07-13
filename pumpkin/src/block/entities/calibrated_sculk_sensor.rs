use super::sculk_sensor::PendingVibration;
use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::world::World;

fn read_pending(nbt: &NbtCompound) -> Option<PendingVibration> {
    let listener = nbt.get_compound("listener")?;
    let delay = listener.get_int("event_delay").unwrap_or(0);
    if delay <= 0 {
        return None;
    }
    let event = listener.get_compound("event")?;
    let pos = event.get_list("pos")?;
    if pos.len() < 3 {
        return None;
    }
    let x = pos[0].extract_double().unwrap_or(0.0);
    let y = pos[1].extract_double().unwrap_or(0.0);
    let z = pos[2].extract_double().unwrap_or(0.0);
    let distance = event.get_float("distance").unwrap_or(0.0) as f64;
    let frequency = event
        .get_int("frequency")
        .or_else(|| nbt.get_int("last_vibration_frequency"))
        .unwrap_or(1)
        .clamp(1, 15) as u8;
    Some(PendingVibration {
        source: Vector3::new(x, y, z),
        frequency,
        distance,
        delay_ticks: delay as u32,
        from_player: false,
    })
}

pub struct CalibratedSculkSensorBlockEntity {
    pub position: BlockPos,
    pub last_vibration_frequency: Mutex<i32>,
    pub pending: Mutex<Option<PendingVibration>>,
}

impl BlockEntity for CalibratedSculkSensorBlockEntity {
    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let last_vibration_frequency = nbt.get_int("last_vibration_frequency").unwrap_or(0);
        Self {
            position,
            last_vibration_frequency: Mutex::new(last_vibration_frequency),
            pending: Mutex::new(read_pending(nbt)),
        }
    }

    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let freq = *self.last_vibration_frequency.lock().await;
            nbt.put_int("last_vibration_frequency", freq);
            if let Some(pending) = self.pending.lock().await.as_ref() {
                let mut listener = NbtCompound::new();
                listener.put_int("event_delay", pending.delay_ticks as i32);
                let mut event = NbtCompound::new();
                event.put_float("distance", pending.distance as f32);
                event.put_int("frequency", i32::from(pending.frequency));
                event.put_list(
                    "pos",
                    vec![
                        NbtTag::Double(pending.source.x),
                        NbtTag::Double(pending.source.y),
                        NbtTag::Double(pending.source.z),
                    ],
                );
                listener.put_compound("event", event);
                nbt.put_compound("listener", listener);
            }
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn tick<'a>(&'a self, world: &'a Arc<World>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let maybe_activate = {
                let mut pending = self.pending.lock().await;
                if let Some(ref mut vib) = *pending {
                    if vib.delay_ticks > 0 {
                        vib.delay_ticks -= 1;
                    }
                    if vib.delay_ticks == 0 {
                        pending.take()
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(vib) = maybe_activate {
                world
                    .activate_sculk_sensor(
                        self.position,
                        vib.frequency,
                        vib.distance,
                        vib.from_player,
                    )
                    .await;
            }
        })
    }
}

impl CalibratedSculkSensorBlockEntity {
    pub const ID: &'static str = "minecraft:calibrated_sculk_sensor";

    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            last_vibration_frequency: Mutex::new(0),
            pending: Mutex::new(None),
        }
    }

    pub async fn try_queue_vibration(
        &self,
        source: Vector3<f64>,
        frequency: u8,
        distance: f64,
        from_player: bool,
    ) -> bool {
        let mut pending = self.pending.lock().await;
        if pending.is_some() {
            return false;
        }
        let delay = distance.ceil().max(0.0) as u32;
        *pending = Some(PendingVibration {
            source,
            frequency,
            distance,
            delay_ticks: delay.max(1),
            from_player,
        });
        true
    }
}
