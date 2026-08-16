//! RL Stats API client, reads live match events off the game's local tcp socket.

pub mod client;
pub mod models;
pub mod parser;
pub mod websocket;

pub use client::StatsClient;
pub use models::*;
pub use parser::{extract_json_objects, parse_message};
pub use websocket::{WsCommand, WsStatsClient};
