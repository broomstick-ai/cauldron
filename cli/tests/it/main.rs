//! The one integration-test binary for `cauldron` (DG-5). Each module is one
//! former `tests/*.rs` file. A new integration test is a new module here,
//! never a new top-level `tests/*.rs`.

mod broken_declaration;
mod c7f4_pin_lock;
mod c7f5_manifest_race;
mod c7f6_env_create_lock;
mod c7f7_create_cleanup_lock;
mod c7f7_relaunch_reads_migrating;
mod c7f7_rollback_owed;
mod daemon_owned_rows;
mod demo_env_seed;
mod drain_pin;
mod env_lifecycle;
mod env_remove_drain;
mod environments;
mod face_env;
mod held_stop_grace;
mod hosts_all;
mod launcher;
mod log_rotation;
mod own_launch_path;
mod owned_live_pid;
mod permissions_cargo_built;
mod pin_revives_parked;
mod quiet_repin;
mod repin_deferral_why;
mod restart_unhealthy_drains;
mod self_stamp;
mod stop_by_pid;
mod stop_via_daemon;
mod supervisor_restarts;
