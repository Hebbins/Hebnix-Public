//! typed models for the RL Stats API events.

use serde::{Deserialize, Serialize};

// primitives

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerRef {
    pub name: String,
    pub shortcut: i64,
    pub team_num: i64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BallState {
    pub speed: f64,
    pub team_num: i64,
}

impl Default for BallState {
    fn default() -> Self {
        Self {
            speed: 0.0,
            team_num: 255,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamState {
    pub name: String,
    pub team_num: i64,
    pub score: i64,
    pub color_primary: String,
    pub color_secondary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetState {
    pub name: String,
    pub shortcut: i64,
    pub team_num: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BallTouch {
    pub player: PlayerRef,
    pub speed: f64,
}

// player (UpdateState)

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerState {
    pub name: String,
    pub primary_id: String,
    pub shortcut: i64,
    pub team_num: i64,
    pub score: i64,
    pub goals: i64,
    pub shots: i64,
    pub assists: i64,
    pub saves: i64,
    pub touches: i64,
    pub car_touches: i64,
    pub demos: i64,
    // spectator-only
    pub has_car: bool,
    pub speed: f64,
    pub boost: i64,
    pub boosting: bool,
    pub on_ground: bool,
    pub on_wall: bool,
    pub powersliding: bool,
    pub demolished: bool,
    pub supersonic: bool,
    pub attacker: Option<PlayerRef>,
}

// Game (UpdateState)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameState {
    pub teams: Vec<TeamState>,
    pub time_seconds: i64,
    pub overtime: bool,
    pub ball: BallState,
    pub replay: bool,
    pub has_winner: bool,
    pub winner: String,
    pub arena: String,
    pub has_target: bool,
    pub target: Option<TargetState>,
    pub frame: i64,
    pub elapsed: f64,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            teams: Vec::new(),
            time_seconds: 300,
            overtime: false,
            ball: BallState::default(),
            replay: false,
            has_winner: false,
            winner: String::new(),
            arena: String::new(),
            has_target: false,
            target: None,
            frame: 0,
            elapsed: 0.0,
        }
    }
}

// Typed event payloads

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateStateData {
    pub players: Vec<PlayerState>,
    pub game: GameState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BallHitData {
    pub players: Vec<PlayerRef>,
    pub ball_pre_hit_speed: f64,
    pub ball_post_hit_speed: f64,
    pub ball_location: Vector3,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrossbarHitData {
    pub ball_speed: f64,
    pub impact_force: f64,
    pub ball_location: Vector3,
    pub ball_last_touch: BallTouch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClockUpdatedSecondsData {
    pub time_seconds: i64,
    pub overtime: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoalScoredData {
    pub goal_speed: f64,
    pub goal_time: f64,
    pub impact_location: Vector3,
    pub scorer: PlayerRef,
    pub assister: Option<PlayerRef>,
    pub ball_last_touch: BallTouch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatfeedData {
    /// e.g. "Demolish"
    pub stat_name: String,
    /// e.g. "Demolition"
    pub stat_type: String,
    pub main_target: PlayerRef,
    pub secondary_target: Option<PlayerRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchEndedData {
    pub winner_team_num: i64,
}

impl Default for MatchEndedData {
    fn default() -> Self {
        Self {
            winner_team_num: -1,
        }
    }
}

/// typed payload of a stats event. Simple = events with nothing beyond
/// MatchGuid (CountdownBegin, GoalReplayStart/End, MatchCreated, etc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventData {
    UpdateState(UpdateStateData),
    BallHit(BallHitData),
    CrossbarHit(CrossbarHitData),
    ClockUpdatedSeconds(ClockUpdatedSecondsData),
    GoalScored(GoalScoredData),
    Statfeed(StatfeedData),
    MatchEnded(MatchEndedData),
    Simple,
}

/// a parsed stats event: type name + typed payload + raw json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsEvent {
    pub event_type: String,
    pub match_guid: Option<String>,
    /// raw "Data" object as the game sent it
    pub raw_data: serde_json::Value,
    pub data: EventData,
}

impl StatsEvent {
    pub fn simple(event_type: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            match_guid: None,
            raw_data: serde_json::Value::Null,
            data: EventData::Simple,
        }
    }

    pub fn update_state(&self) -> Option<&UpdateStateData> {
        match &self.data {
            EventData::UpdateState(d) => Some(d),
            _ => None,
        }
    }

    pub fn goal_scored(&self) -> Option<&GoalScoredData> {
        match &self.data {
            EventData::GoalScored(d) => Some(d),
            _ => None,
        }
    }
}
