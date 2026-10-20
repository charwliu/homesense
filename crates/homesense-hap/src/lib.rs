//! Native HAP adapter. Services are started explicitly; constructing storage
//! never broadcasts, pairs, or connects to a physical gateway.
pub mod away_graph;
pub mod events;
pub mod graph;
pub mod identifiers;
pub mod observed;
pub mod requests;
pub mod storage;
pub mod write;
pub mod scene_graph;

pub mod scene_requests;
pub mod scene_write;

pub mod write_router;

pub mod motor_graph;
pub mod motor_requests;

pub mod window_covering_graph;
pub mod window_covering_requests;

pub mod panel_graph;

pub mod panel_input;

pub mod panel_events;
