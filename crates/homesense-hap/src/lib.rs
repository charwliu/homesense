//! Native HAP adapter. Services are started explicitly; constructing storage
//! never broadcasts, pairs, or connects to a physical gateway.
pub mod requests;
pub mod storage;
