//! Runtime components; construction does not start a listener or connect hardware.
pub mod admission;
pub mod simulation;
pub mod simulation_hap;

mod light_command;

pub mod transport_owner;

pub mod feedback;

pub mod observed_transport;

pub mod runtime_hap;

pub mod service;

pub mod diagnostics;

pub mod read_probe;

pub mod parameter_probe;

pub mod reconnect;

pub mod live_service;

pub mod combined_light;

pub mod configuration_publish;

mod restore_policy;

pub mod panel_led;

pub mod delayed_actions;

pub mod motor_commission;

pub mod study_runtime;

pub mod study_commission;

pub mod scene_runtime;

pub mod rule_runtime;

pub mod panel_rules;

pub mod hold_dimming;

pub mod scene_scheduler;

pub mod automation_drive;

pub mod event_rules;

pub mod configured_rules;

pub mod scene_activity;

pub mod automation_runtime;
pub mod automation_control;
pub mod publication_recovery;

pub mod deferred_rule;

pub mod deferred_session;

pub mod deferred_router;

pub mod automation_tcp;

pub mod automation_publication;

pub mod software_fade;

pub mod fade_group;

mod fade_budget;

pub mod runtime_scene_hap;

pub mod web_auth;
pub mod web_credentials;
pub mod web_receipts;
pub mod web_api;
pub mod web_server;

pub mod unified_hap;

pub mod management_devices;

pub mod web_drafts;

pub mod configuration_preview;

pub mod web_activation;

mod web_supervision;

pub mod management_events;

mod web_events;

pub mod web_tls;

mod management_endpoint;

pub mod hap_endpoint;
pub mod hap_bridge_plan;
pub mod hap_bridges;
pub mod hap_bridge_group;
pub mod runtime_motor_hap;

mod web_history;
mod web_scene_history;
mod web_run_details;

pub mod daily_backup;

pub mod web_backups;

pub mod runtime_panel_hap;

pub use fade_budget::Capacity as WireCapacity;

pub mod scene_switches;

mod scene_selection_hap;

pub mod light_state;

pub mod indicator_sync;

mod scene_observation_owner;
mod scene_observation_runtime;
