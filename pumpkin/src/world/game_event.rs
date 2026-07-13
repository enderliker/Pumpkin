//! Server-side game events / vibrations for sculk sensors.
//!
//! Distinct from the client play `CGameEvent` packet (weather, gamemode, etc.).

use std::sync::Arc;

use pumpkin_data::block_properties::{
    BlockProperties, CalibratedSculkSensorLikeProperties, SculkSensorLikeProperties,
    SculkSensorPhase,
};
use pumpkin_data::game_event::GameEvent;
use pumpkin_data::particle::Particle;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::{Block, BlockDirection, BlockId, HorizontalFacingExt};
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::ser::NetworkWriteExt;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;

use crate::block::blocks::redstone::get_redstone_power;
use crate::block::blocks::redstone::sculk_sensor::SculkSensorBlock;
use crate::block::entities::calibrated_sculk_sensor::CalibratedSculkSensorBlockEntity;
use crate::block::entities::sculk_sensor::SculkSensorBlockEntity;
use crate::world::World;

/// Detection range of a normal sculk sensor (blocks, spherical).
pub const SCULK_SENSOR_RANGE: f64 = 8.0;
/// Detection range of a calibrated sculk sensor.
pub const CALIBRATED_SCULK_SENSOR_RANGE: f64 = 16.0;

pub const SCULK_SENSOR_ACTIVE_TICKS: u8 = 30;
pub const SCULK_SENSOR_COOLDOWN_TICKS: u8 = 10;
pub const CALIBRATED_ACTIVE_TICKS: u8 = 10;
pub const CALIBRATED_COOLDOWN_TICKS: u8 = 10;

/// Who/what caused the vibration (used for JE sculk/warden filtering and shrieker alerts).
#[derive(Debug, Clone, Copy, Default)]
pub struct VibrationSource {
    pub is_player: bool,
    pub is_sneaking: bool,
    /// Warden or sculk-family emitters are ignored by sensors in Java Edition.
    pub is_sculk_or_warden: bool,
}

impl VibrationSource {
    pub const PLAYER: Self = Self {
        is_player: true,
        is_sneaking: false,
        is_sculk_or_warden: false,
    };

    pub const PLAYER_SNEAKING: Self = Self {
        is_player: true,
        is_sneaking: true,
        is_sculk_or_warden: false,
    };

    pub const NONE: Self = Self {
        is_player: false,
        is_sneaking: false,
        is_sculk_or_warden: false,
    };
}

/// Comparator / frequency output for a game event (1–15).
#[must_use]
pub fn game_event_frequency(event: &GameEvent) -> u8 {
    match event {
        GameEvent::Step
        | GameEvent::Swim
        | GameEvent::Flap
        | GameEvent::Resonate1 => 1,
        GameEvent::ProjectileLand
        | GameEvent::HitGround
        | GameEvent::Bounce
        | GameEvent::Splash
        | GameEvent::Resonate2 => 2,
        GameEvent::ItemInteractFinish
        | GameEvent::ProjectileShoot
        | GameEvent::InstrumentPlay
        | GameEvent::Resonate3 => 3,
        GameEvent::EntityAction
        | GameEvent::ElytraGlide
        | GameEvent::Unequip
        | GameEvent::Resonate4 => 4,
        GameEvent::EntityDismount | GameEvent::Equip | GameEvent::Resonate5 => 5,
        GameEvent::EntityMount
        | GameEvent::EntityInteract
        | GameEvent::Shear
        | GameEvent::Resonate6 => 6,
        GameEvent::EntityDamage | GameEvent::Resonate7 => 7,
        GameEvent::Drink | GameEvent::Eat | GameEvent::Resonate8 => 8,
        GameEvent::ContainerClose
        | GameEvent::BlockClose
        | GameEvent::BlockDeactivate
        | GameEvent::BlockDetach
        | GameEvent::Resonate9 => 9,
        GameEvent::ContainerOpen
        | GameEvent::BlockOpen
        | GameEvent::BlockActivate
        | GameEvent::BlockAttach
        | GameEvent::PrimeFuse
        | GameEvent::NoteBlockPlay
        | GameEvent::Resonate10 => 10,
        GameEvent::BlockChange | GameEvent::Resonate11 => 11,
        GameEvent::BlockDestroy | GameEvent::FluidPickup | GameEvent::Resonate12 => 12,
        GameEvent::BlockPlace | GameEvent::FluidPlace | GameEvent::Resonate13 => 13,
        GameEvent::EntityPlace
        | GameEvent::LightningStrike
        | GameEvent::Teleport
        | GameEvent::Resonate14 => 14,
        GameEvent::EntityDie | GameEvent::Explode | GameEvent::Resonate15 => 15,
        // Sensor self-click / shriek are not standard sensor listenables via vibrations tag.
        GameEvent::SculkSensorTendrilsClicking
        | GameEvent::Shriek
        | GameEvent::ItemInteractStart
        | GameEvent::JukeboxPlay
        | GameEvent::JukeboxStopPlay => 0,
    }
}

/// Events suppressed while a player is sneaking (tag `ignore_vibrations_sneaking`).
#[must_use]
pub fn ignored_when_sneaking(event: &GameEvent) -> bool {
    matches!(
        event,
        GameEvent::HitGround
            | GameEvent::ProjectileShoot
            | GameEvent::Step
            | GameEvent::Swim
            | GameEvent::ItemInteractStart
            | GameEvent::ItemInteractFinish
    )
}

/// Distance-based redstone strength for an active sensor.
#[must_use]
pub fn redstone_strength(distance: f64, range: f64) -> u8 {
    if range <= 0.0 {
        return 1;
    }
    let v = 15.0 - ((15.0 / range) * distance).floor();
    v.max(1.0).min(15.0) as u8
}

/// Euclidean distance between a position and a block center.
#[must_use]
pub fn center_distance(pos: Vector3<f64>, block: BlockPos) -> f64 {
    let bc = block.to_centered_f64();
    let dx = pos.x - bc.x;
    let dy = pos.y - bc.y;
    let dz = pos.z - bc.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// True if the ray between block centers is occluded by a `dampens_vibrations` block.
pub fn is_vibration_occluded(world: &World, from: BlockPos, to: BlockPos) -> bool {
    if from == to {
        return false;
    }
    let start = from.to_centered_f64();
    let end = to.to_centered_f64();
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let dz = end.z - start.z;
    let len = (dx * dx + dy * dy + dz * dz).sqrt();
    if len < f64::EPSILON {
        return false;
    }

    // DDA / stepped raycast along the segment (skip endpoints).
    let steps = (len * 2.0).ceil().max(1.0) as i32;
    let mut last: Option<BlockPos> = None;
    for i in 1..steps {
        let t = f64::from(i) / f64::from(steps);
        let x = start.x + dx * t;
        let y = start.y + dy * t;
        let z = start.z + dz * t;
        let pos = BlockPos(Vector3::new(
            x.floor() as i32,
            y.floor() as i32,
            z.floor() as i32,
        ));
        if last == Some(pos) || pos == from || pos == to {
            continue;
        }
        last = Some(pos);
        let block = world.get_block(&pos);
        if block.has_tag(&tag::Block::MINECRAFT_DAMPENS_VIBRATIONS)
            || block.has_tag(&tag::Block::MINECRAFT_OCCLUDES_VIBRATION_SIGNALS)
        {
            return true;
        }
    }
    false
}

/// Whether the source block itself damps vibrations (wool / carpet place, step, etc.).
#[must_use]
pub fn source_dampens_vibrations(block: &Block) -> bool {
    block.has_tag(&tag::Block::MINECRAFT_DAMPENS_VIBRATIONS)
}

/// Encode protocol data for `minecraft:vibration` (block destination).
///
/// Layout (Java Edition protocol):
/// - VarInt position source type (`0` = block)
/// - Position destination (packed `i64`)
/// - VarInt arrival ticks
#[must_use]
pub fn encode_vibration_particle_data(destination: BlockPos, arrival_ticks: i32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    // 0 = minecraft:block position source
    let _ = buf.write_var_int(&VarInt(0));
    let _ = buf.write_block_pos(&destination);
    let _ = buf.write_var_int(&VarInt(arrival_ticks.max(0)));
    buf
}

/// Spawn the cyan vibration wave particle from `source` toward `destination`.
pub fn spawn_vibration_particle(world: &World, source: Vector3<f64>, destination: BlockPos, ticks: u32) {
    let data = encode_vibration_particle_data(destination, ticks as i32);
    // Particle origin = event source (exact pos); travels to the sensor.
    world.spawn_particle_with_data(
        source,
        Vector3::new(0.0, 0.0, 0.0),
        0.0,
        1,
        Particle::Vibration,
        &data,
        true,
        true,
    );
}

fn resonate_event(frequency: u8) -> Option<GameEvent> {
    match frequency {
        1 => Some(GameEvent::Resonate1),
        2 => Some(GameEvent::Resonate2),
        3 => Some(GameEvent::Resonate3),
        4 => Some(GameEvent::Resonate4),
        5 => Some(GameEvent::Resonate5),
        6 => Some(GameEvent::Resonate6),
        7 => Some(GameEvent::Resonate7),
        8 => Some(GameEvent::Resonate8),
        9 => Some(GameEvent::Resonate9),
        10 => Some(GameEvent::Resonate10),
        11 => Some(GameEvent::Resonate11),
        12 => Some(GameEvent::Resonate12),
        13 => Some(GameEvent::Resonate13),
        14 => Some(GameEvent::Resonate14),
        15 => Some(GameEvent::Resonate15),
        _ => None,
    }
}

impl World {
    /// Emit a game event (vibration) at `pos` and notify nearby sculk sensors.
    pub async fn emit_game_event(
        self: &Arc<Self>,
        pos: Vector3<f64>,
        event: GameEvent,
        source: VibrationSource,
    ) {
        if source.is_sculk_or_warden {
            return;
        }

        if source.is_sneaking && ignored_when_sneaking(&event) {
            return;
        }

        let frequency = game_event_frequency(&event);
        if frequency == 0 {
            return;
        }

        let center_block = BlockPos(Vector3::new(
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        ));

        // Wool/carpet at the source for place/destroy/step-like events.
        let source_block = self.get_block(&center_block);
        if source_dampens_vibrations(source_block)
            && matches!(
                event,
                GameEvent::BlockPlace
                    | GameEvent::BlockDestroy
                    | GameEvent::Step
                    | GameEvent::HitGround
                    | GameEvent::ProjectileLand
            )
        {
            return;
        }

        // Scan loaded block entities for sensors in range of the maximum (calibrated = 16).
        let range_i = CALIBRATED_SCULK_SENSOR_RANGE.ceil() as i32;
        let active_chunks = self.active_chunks.load();
        let mut candidates: Vec<(BlockPos, bool)> = Vec::new();

        for dx in -range_i..=range_i {
            for dy in -range_i..=range_i {
                for dz in -range_i..=range_i {
                    let sensor_pos = BlockPos(Vector3::new(
                        center_block.0.x + dx,
                        center_block.0.y + dy,
                        center_block.0.z + dz,
                    ));
                    let dist = center_distance(pos, sensor_pos);
                    if dist > CALIBRATED_SCULK_SENSOR_RANGE {
                        continue;
                    }
                    let chunk = sensor_pos.chunk_position();
                    if !active_chunks.contains(&chunk) {
                        continue;
                    }
                    let block = self.get_block(&sensor_pos);
                    match block.id {
                        BlockId::SCULK_SENSOR if dist <= SCULK_SENSOR_RANGE => {
                            candidates.push((sensor_pos, false));
                        }
                        BlockId::CALIBRATED_SCULK_SENSOR => {
                            candidates.push((sensor_pos, true));
                        }
                        _ => {}
                    }
                }
            }
        }

        for (sensor_pos, calibrated) in candidates {
            if is_vibration_occluded(self, center_block, sensor_pos) {
                continue;
            }
            self.try_sensor_accept_vibration(
                sensor_pos,
                calibrated,
                pos,
                frequency,
                source.is_player,
            )
            .await;
        }

        let _ = event;
    }

    async fn try_sensor_accept_vibration(
        self: &Arc<Self>,
        sensor_pos: BlockPos,
        calibrated: bool,
        source_pos: Vector3<f64>,
        frequency: u8,
        from_player: bool,
    ) {
        let range = if calibrated {
            CALIBRATED_SCULK_SENSOR_RANGE
        } else {
            SCULK_SENSOR_RANGE
        };
        let distance = center_distance(source_pos, sensor_pos);
        if distance > range {
            return;
        }

        let (block, state) = self.get_block_and_state(&sensor_pos);
        let phase = if calibrated {
            CalibratedSculkSensorLikeProperties::from_state_id(state.id, block).sculk_sensor_phase
        } else {
            SculkSensorLikeProperties::from_state_id(state.id, block).sculk_sensor_phase
        };
        if phase != SculkSensorPhase::Inactive {
            return;
        }

        // Calibrated frequency filter from redstone into the amethyst (facing) side.
        if calibrated {
            let props = CalibratedSculkSensorLikeProperties::from_state_id(state.id, block);
            let facing_dir = props.facing.to_block_direction();
            let input_pos = sensor_pos.offset(facing_dir.to_offset());
            let (in_block, in_state) = self.get_block_and_state(&input_pos);
            // Power coming into the sensor from the facing neighbor.
            let calibration = get_redstone_power(
                in_block,
                in_state,
                self,
                &input_pos,
                facing_dir.opposite(),
            )
            .await;
            if calibration > 0 && calibration != frequency {
                return;
            }
        }

        // Prefer block entity queue so travel delay is handled on tick.
        let delay_ticks = distance.ceil().max(1.0) as u32;
        let accepted = if let Some(be) = self.get_block_entity(&sensor_pos) {
            if calibrated {
                if let Some(sensor) = be
                    .as_any()
                    .downcast_ref::<CalibratedSculkSensorBlockEntity>()
                {
                    sensor
                        .try_queue_vibration(source_pos, frequency, distance, from_player)
                        .await
                } else {
                    false
                }
            } else if let Some(sensor) = be.as_any().downcast_ref::<SculkSensorBlockEntity>() {
                sensor
                    .try_queue_vibration(source_pos, frequency, distance, from_player)
                    .await
            } else {
                false
            }
        } else {
            // No BE yet: activate immediately (fallback).
            let power = redstone_strength(distance, range);
            SculkSensorBlock::trigger(self, &sensor_pos, block, power, frequency).await;
            true
        };

        if accepted {
            // Vanilla cyan wave from the event origin toward the sensor.
            spawn_vibration_particle(self, source_pos, sensor_pos, delay_ticks);
        }
    }

    /// Called when a vibration arrives at a sensor (after travel delay).
    pub async fn activate_sculk_sensor(
        self: &Arc<Self>,
        sensor_pos: BlockPos,
        frequency: u8,
        distance: f64,
        from_player: bool,
    ) {
        let (block, _state) = self.get_block_and_state(&sensor_pos);
        let range = if block.id == BlockId::CALIBRATED_SCULK_SENSOR {
            CALIBRATED_SCULK_SENSOR_RANGE
        } else {
            SCULK_SENSOR_RANGE
        };
        let power = redstone_strength(distance, range);

        if let Some(be) = self.get_block_entity(&sensor_pos) {
            if let Some(sensor) = be.as_any().downcast_ref::<SculkSensorBlockEntity>() {
                *sensor.last_vibration_frequency.lock().await = i32::from(frequency);
            } else if let Some(sensor) = be
                .as_any()
                .downcast_ref::<CalibratedSculkSensorBlockEntity>()
            {
                *sensor.last_vibration_frequency.lock().await = i32::from(frequency);
            }
        }

        SculkSensorBlock::trigger(self, &sensor_pos, block, power, frequency).await;

        // Amethyst resonance: re-emit at adjacent resonators.
        if resonate_event(frequency).is_some() {
            for dir in BlockDirection::all() {
                let neighbor = sensor_pos.offset(dir.to_offset());
                let nblock = self.get_block(&neighbor);
                if nblock.has_tag(&tag::Block::MINECRAFT_VIBRATION_RESONATORS)
                    || nblock.id == BlockId::AMETHYST_BLOCK
                {
                    // Re-resolve each iteration (GameEvent is not Copy).
                    if let Some(event) = resonate_event(frequency) {
                        self.emit_game_event(neighbor.to_centered_f64(), event, VibrationSource::NONE)
                            .await;
                    }
                }
            }
        }

        let _ = from_player; // shrieker wiring can use this later
    }
}

/// Play sensor click sound if not waterlogged.
pub fn play_sensor_click(world: &World, pos: &BlockPos, waterlogged: bool) {
    if waterlogged {
        return;
    }
    world.play_block_sound(
        Sound::BlockSculkSensorClicking,
        SoundCategory::Blocks,
        *pos,
    );
}

pub fn play_sensor_click_stop(world: &World, pos: &BlockPos, waterlogged: bool) {
    if waterlogged {
        return;
    }
    world.play_block_sound(
        Sound::BlockSculkSensorClickingStop,
        SoundCategory::Blocks,
        *pos,
    );
}
