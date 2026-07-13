use std::sync::Arc;

use crate::block::entities::calibrated_sculk_sensor::CalibratedSculkSensorBlockEntity;
use crate::block::entities::sculk_sensor::SculkSensorBlockEntity;
use crate::block::{
    BlockBehaviour, BlockFuture, BlockMetadata, BrokenArgs, EmitsRedstonePowerArgs,
    GetComparatorOutputArgs, GetRedstonePowerArgs, OnPlaceArgs, OnScheduledTickArgs, PlacedArgs,
};
use crate::world::game_event::{
    play_sensor_click, play_sensor_click_stop, CALIBRATED_ACTIVE_TICKS, CALIBRATED_COOLDOWN_TICKS,
    SCULK_SENSOR_ACTIVE_TICKS, SCULK_SENSOR_COOLDOWN_TICKS,
};
use crate::world::World;
use pumpkin_data::block_properties::{
    BlockProperties, CalibratedSculkSensorLikeProperties, SculkSensorLikeProperties,
    SculkSensorPhase,
};
use pumpkin_data::{Block, BlockDirection, BlockId, BlockStateId, HorizontalFacingExt};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::tick::TickPriority;
use pumpkin_world::world::BlockFlags;

pub struct SculkSensorBlock;

impl BlockMetadata for SculkSensorBlock {
    fn ids() -> Box<[BlockId]> {
        [BlockId::SCULK_SENSOR, BlockId::CALIBRATED_SCULK_SENSOR].into()
    }
}

impl SculkSensorBlock {
    /// Activate a sensor that is currently inactive (power = distance-based strength).
    pub async fn trigger(
        world: &Arc<World>,
        pos: &BlockPos,
        block: &Block,
        power: u8,
        frequency: u8,
    ) {
        if let Some(be) = world.get_block_entity(pos) {
            if let Some(sensor) = be.as_any().downcast_ref::<SculkSensorBlockEntity>() {
                *sensor.last_vibration_frequency.lock().await = i32::from(frequency);
            } else if let Some(sensor) = be
                .as_any()
                .downcast_ref::<CalibratedSculkSensorBlockEntity>()
            {
                *sensor.last_vibration_frequency.lock().await = i32::from(frequency);
            }
        }

        if block.id == BlockId::SCULK_SENSOR {
            let state = world.get_block_state(pos);
            let mut props = SculkSensorLikeProperties::from_state_id(state.id, block);
            if props.sculk_sensor_phase != SculkSensorPhase::Inactive {
                return;
            }
            props.sculk_sensor_phase = SculkSensorPhase::Active;
            props.power = power.clamp(1, 15);
            play_sensor_click(world, pos, props.waterlogged);
            world
                .set_block_state(pos, props.to_state_id(block), BlockFlags::NOTIFY_ALL)
                .await;
            world.update_neighbors(pos, None).await;
            world.schedule_block_tick(
                block,
                *pos,
                SCULK_SENSOR_ACTIVE_TICKS,
                TickPriority::Normal,
            );
        } else if block.id == BlockId::CALIBRATED_SCULK_SENSOR {
            let state = world.get_block_state(pos);
            let mut props = CalibratedSculkSensorLikeProperties::from_state_id(state.id, block);
            if props.sculk_sensor_phase != SculkSensorPhase::Inactive {
                return;
            }
            props.sculk_sensor_phase = SculkSensorPhase::Active;
            props.power = power.clamp(1, 15);
            play_sensor_click(world, pos, props.waterlogged);
            world
                .set_block_state(pos, props.to_state_id(block), BlockFlags::NOTIFY_ALL)
                .await;
            world.update_neighbors(pos, None).await;
            world.schedule_block_tick(
                block,
                *pos,
                CALIBRATED_ACTIVE_TICKS,
                TickPriority::Normal,
            );
        }
    }
}

impl BlockBehaviour for SculkSensorBlock {
    fn on_place<'a>(&'a self, args: OnPlaceArgs<'a>) -> BlockFuture<'a, BlockStateId> {
        Box::pin(async move {
            let waterlogged = args.replacing.water_source();

            if args.block.id == BlockId::CALIBRATED_SCULK_SENSOR {
                let mut props = CalibratedSculkSensorLikeProperties::default(args.block);
                props.facing = args.player.living_entity.entity.get_horizontal_facing();
                props.waterlogged = waterlogged;
                // Start in cooldown so placement/self-power does not immediately re-trigger.
                props.sculk_sensor_phase = SculkSensorPhase::Cooldown;
                props.to_state_id(args.block)
            } else {
                let mut props = SculkSensorLikeProperties::default(args.block);
                props.waterlogged = waterlogged;
                props.sculk_sensor_phase = SculkSensorPhase::Cooldown;
                props.to_state_id(args.block)
            }
        })
    }

    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            // Cooldown after place (10 ticks).
            args.world.schedule_block_tick(
                args.block,
                *args.position,
                SCULK_SENSOR_COOLDOWN_TICKS,
                TickPriority::Normal,
            );
        })
    }

    fn broken<'a>(&'a self, args: BrokenArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            args.world.remove_block_entity(args.position);
        })
    }

    fn emits_redstone_power<'a>(
        &'a self,
        _args: EmitsRedstonePowerArgs<'a>,
    ) -> BlockFuture<'a, bool> {
        Box::pin(async move { true })
    }

    fn get_weak_redstone_power<'a>(
        &'a self,
        args: GetRedstonePowerArgs<'a>,
    ) -> BlockFuture<'a, u8> {
        Box::pin(async move {
            if args.block.id == BlockId::SCULK_SENSOR {
                let props = SculkSensorLikeProperties::from_state_id(args.state.id, args.block);
                if props.sculk_sensor_phase == SculkSensorPhase::Active {
                    props.power
                } else {
                    0
                }
            } else if args.block.id == BlockId::CALIBRATED_SCULK_SENSOR {
                let props =
                    CalibratedSculkSensorLikeProperties::from_state_id(args.state.id, args.block);
                // Input (amethyst) face does not emit.
                if args.direction == props.facing.opposite().to_block_direction() {
                    return 0;
                }
                if props.sculk_sensor_phase == SculkSensorPhase::Active {
                    props.power
                } else {
                    0
                }
            } else {
                0
            }
        })
    }

    fn get_strong_redstone_power<'a>(
        &'a self,
        args: GetRedstonePowerArgs<'a>,
    ) -> BlockFuture<'a, u8> {
        Box::pin(async move {
            // Strongly powers the block below (queried with direction Up from that block).
            if args.direction != BlockDirection::Up {
                return 0;
            }
            if args.block.id == BlockId::SCULK_SENSOR {
                let props = SculkSensorLikeProperties::from_state_id(args.state.id, args.block);
                if props.sculk_sensor_phase == SculkSensorPhase::Active {
                    props.power
                } else {
                    0
                }
            } else if args.block.id == BlockId::CALIBRATED_SCULK_SENSOR {
                let props =
                    CalibratedSculkSensorLikeProperties::from_state_id(args.state.id, args.block);
                if props.sculk_sensor_phase == SculkSensorPhase::Active {
                    props.power
                } else {
                    0
                }
            } else {
                0
            }
        })
    }

    fn get_comparator_output<'a>(
        &'a self,
        args: GetComparatorOutputArgs<'a>,
    ) -> BlockFuture<'a, Option<u8>> {
        Box::pin(async move {
            if let Some(be) = args.world.get_block_entity(args.position) {
                if let Some(sensor) = be.as_any().downcast_ref::<SculkSensorBlockEntity>() {
                    let freq = *sensor.last_vibration_frequency.lock().await;
                    return Some(freq.clamp(0, 15) as u8);
                }
                if let Some(sensor) = be
                    .as_any()
                    .downcast_ref::<CalibratedSculkSensorBlockEntity>()
                {
                    let freq = *sensor.last_vibration_frequency.lock().await;
                    return Some(freq.clamp(0, 15) as u8);
                }
            }
            Some(0)
        })
    }

    fn on_scheduled_tick<'a>(&'a self, args: OnScheduledTickArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let state = args.world.get_block_state(args.position);
            if args.block.id == BlockId::SCULK_SENSOR {
                let mut props = SculkSensorLikeProperties::from_state_id(state.id, args.block);
                match props.sculk_sensor_phase {
                    SculkSensorPhase::Active => {
                        play_sensor_click_stop(args.world, args.position, props.waterlogged);
                        props.sculk_sensor_phase = SculkSensorPhase::Cooldown;
                        props.power = 0;
                        args.world
                            .set_block_state(
                                args.position,
                                props.to_state_id(args.block),
                                BlockFlags::NOTIFY_ALL,
                            )
                            .await;
                        args.world.schedule_block_tick(
                            args.block,
                            *args.position,
                            SCULK_SENSOR_COOLDOWN_TICKS,
                            TickPriority::Normal,
                        );
                        args.world.update_neighbors(args.position, None).await;
                    }
                    SculkSensorPhase::Cooldown => {
                        props.sculk_sensor_phase = SculkSensorPhase::Inactive;
                        props.power = 0;
                        args.world
                            .set_block_state(
                                args.position,
                                props.to_state_id(args.block),
                                BlockFlags::NOTIFY_ALL,
                            )
                            .await;
                        args.world.update_neighbors(args.position, None).await;
                    }
                    SculkSensorPhase::Inactive => {}
                }
            } else if args.block.id == BlockId::CALIBRATED_SCULK_SENSOR {
                let mut props =
                    CalibratedSculkSensorLikeProperties::from_state_id(state.id, args.block);
                match props.sculk_sensor_phase {
                    SculkSensorPhase::Active => {
                        play_sensor_click_stop(args.world, args.position, props.waterlogged);
                        props.sculk_sensor_phase = SculkSensorPhase::Cooldown;
                        props.power = 0;
                        args.world
                            .set_block_state(
                                args.position,
                                props.to_state_id(args.block),
                                BlockFlags::NOTIFY_ALL,
                            )
                            .await;
                        args.world.schedule_block_tick(
                            args.block,
                            *args.position,
                            CALIBRATED_COOLDOWN_TICKS,
                            TickPriority::Normal,
                        );
                        args.world.update_neighbors(args.position, None).await;
                    }
                    SculkSensorPhase::Cooldown => {
                        props.sculk_sensor_phase = SculkSensorPhase::Inactive;
                        props.power = 0;
                        args.world
                            .set_block_state(
                                args.position,
                                props.to_state_id(args.block),
                                BlockFlags::NOTIFY_ALL,
                            )
                            .await;
                        args.world.update_neighbors(args.position, None).await;
                    }
                    SculkSensorPhase::Inactive => {}
                }
            }
        })
    }
}
