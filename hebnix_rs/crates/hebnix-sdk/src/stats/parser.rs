//! json extraction + stats api event parsing.

use serde_json::Value;

use crate::stats::models::*;

/// pull complete json objects out of a raw tcp buffer. returns the objects +
/// the leftover unparsed tail. handles nested braces + escaped quotes.
pub fn extract_json_objects(buf: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let mut i = 0usize;

    while i < buf.len() {
        if buf[i] == b'{' {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut escape = false;
            let mut j = i;
            let mut closed = false;

            while j < buf.len() {
                let c = buf[j];
                if escape {
                    escape = false;
                } else if c == b'\\' {
                    escape = true;
                } else if c == b'"' {
                    in_str = !in_str;
                } else if !in_str {
                    if c == b'{' {
                        depth += 1;
                    } else if c == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            objects.push(buf[i..=j].to_vec());
                            i = j + 1;
                            closed = true;
                            break;
                        }
                    }
                }
                j += 1;
            }
            if !closed {
                // incomplete object at end of buffer
                break;
            }
        } else {
            i += 1;
        }
    }

    (objects, buf[i..].to_vec())
}

/// parse one stats api json message into a typed event
pub fn parse_message(raw: &[u8]) -> Result<StatsEvent, serde_json::Error> {
    let msg: Value = serde_json::from_slice(raw)?;
    let evt = msg
        .get("Event")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // "Data" is usually an object, sometimes a json-encoded string
    let data: Value = match msg.get("Data") {
        Some(Value::String(s)) => {
            serde_json::from_str(s).unwrap_or(Value::Object(Default::default()))
        }
        Some(v) => v.clone(),
        None => Value::Object(Default::default()),
    };

    let match_guid = data
        .get("MatchGuid")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let payload = match evt.as_str() {
        "UpdateState" => EventData::UpdateState(parse_update_state(&data)),
        "BallHit" => EventData::BallHit(parse_ball_hit(&data)),
        "CrossbarHit" => EventData::CrossbarHit(parse_crossbar_hit(&data)),
        "ClockUpdatedSeconds" => EventData::ClockUpdatedSeconds(parse_clock_updated(&data)),
        "GoalScored" => EventData::GoalScored(parse_goal_scored(&data)),
        "StatfeedEvent" => EventData::Statfeed(parse_statfeed(&data)),
        "MatchEnded" => EventData::MatchEnded(parse_match_ended(&data)),
        _ => EventData::Simple,
    };

    Ok(StatsEvent {
        event_type: evt,
        match_guid,
        raw_data: data,
        data: payload,
    })
}

// value helpers, default on missing/wrong type

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn i(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn i_or(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(default)
}

fn f(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn b(v: &Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

// Internal parsers

fn parse_player_ref(d: &Value) -> PlayerRef {
    PlayerRef {
        name: s(d, "Name"),
        shortcut: i(d, "Shortcut"),
        team_num: i(d, "TeamNum"),
    }
}

fn parse_vector3(d: &Value) -> Vector3 {
    Vector3 {
        x: f(d, "X"),
        y: f(d, "Y"),
        z: f(d, "Z"),
    }
}

fn parse_ball_touch(d: &Value) -> BallTouch {
    let empty = Value::Object(Default::default());
    let player = d.get("Player").unwrap_or(&empty);
    BallTouch {
        player: parse_player_ref(player),
        speed: f(d, "Speed"),
    }
}

fn parse_update_state(data: &Value) -> UpdateStateData {
    let mut players = Vec::new();
    if let Some(arr) = data.get("Players").and_then(|v| v.as_array()) {
        for p in arr {
            let attacker = p
                .get("Attacker")
                .filter(|a| a.is_object() && !a.as_object().unwrap().is_empty())
                .map(parse_player_ref);
            players.push(PlayerState {
                name: s(p, "Name"),
                primary_id: s(p, "PrimaryId"),
                shortcut: i(p, "Shortcut"),
                team_num: i(p, "TeamNum"),
                score: i(p, "Score"),
                goals: i(p, "Goals"),
                shots: i(p, "Shots"),
                assists: i(p, "Assists"),
                saves: i(p, "Saves"),
                touches: i(p, "Touches"),
                car_touches: i(p, "CarTouches"),
                demos: i(p, "Demos"),
                has_car: b(p, "bHasCar"),
                speed: f(p, "Speed"),
                boost: i(p, "Boost"),
                boosting: b(p, "bBoosting"),
                on_ground: b(p, "bOnGround"),
                on_wall: b(p, "bOnWall"),
                powersliding: b(p, "bPowersliding"),
                demolished: b(p, "bDemolished"),
                supersonic: b(p, "bSupersonic"),
                attacker,
            });
        }
    }

    let empty = Value::Object(Default::default());
    let game = data.get("Game").unwrap_or(&empty);

    let mut teams = Vec::new();
    if let Some(arr) = game.get("Teams").and_then(|v| v.as_array()) {
        for t in arr {
            teams.push(TeamState {
                name: s(t, "Name"),
                team_num: i(t, "TeamNum"),
                score: i(t, "Score"),
                color_primary: s(t, "ColorPrimary"),
                color_secondary: s(t, "ColorSecondary"),
            });
        }
    }

    let ball = game.get("Ball").unwrap_or(&empty);
    let ball_state = BallState {
        speed: f(ball, "Speed"),
        team_num: i_or(ball, "TeamNum", 255),
    };

    let target = game
        .get("Target")
        .filter(|t| t.is_object() && !t.as_object().unwrap().is_empty())
        .map(|tgt| TargetState {
            name: s(tgt, "Name"),
            shortcut: i(tgt, "Shortcut"),
            team_num: i(tgt, "TeamNum"),
        });

    UpdateStateData {
        players,
        game: GameState {
            teams,
            time_seconds: i_or(game, "TimeSeconds", 300),
            overtime: b(game, "bOvertime"),
            ball: ball_state,
            replay: b(game, "bReplay"),
            has_winner: b(game, "bHasWinner"),
            winner: s(game, "Winner"),
            arena: s(game, "Arena"),
            has_target: b(game, "bHasTarget"),
            target,
            frame: i(game, "Frame"),
            elapsed: f(game, "Elapsed"),
        },
    }
}

fn parse_ball_hit(data: &Value) -> BallHitData {
    let players = data
        .get("Players")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(parse_player_ref).collect())
        .unwrap_or_default();
    let empty = Value::Object(Default::default());
    let ball = data.get("Ball").unwrap_or(&empty);
    let loc = ball.get("Location").unwrap_or(&empty);
    BallHitData {
        players,
        ball_pre_hit_speed: f(ball, "PreHitSpeed"),
        ball_post_hit_speed: f(ball, "PostHitSpeed"),
        ball_location: parse_vector3(loc),
    }
}

fn parse_crossbar_hit(data: &Value) -> CrossbarHitData {
    let empty = Value::Object(Default::default());
    let bt = data.get("BallLastTouch").unwrap_or(&empty);
    let loc = data.get("BallLocation").unwrap_or(&empty);
    CrossbarHitData {
        ball_speed: f(data, "BallSpeed"),
        impact_force: f(data, "ImpactForce"),
        ball_location: parse_vector3(loc),
        ball_last_touch: parse_ball_touch(bt),
    }
}

fn parse_clock_updated(data: &Value) -> ClockUpdatedSecondsData {
    ClockUpdatedSecondsData {
        time_seconds: i(data, "TimeSeconds"),
        overtime: b(data, "bOvertime"),
    }
}

fn parse_goal_scored(data: &Value) -> GoalScoredData {
    let empty = Value::Object(Default::default());
    let assister = data
        .get("Assister")
        .filter(|a| a.is_object() && !a.as_object().unwrap().is_empty())
        .map(parse_player_ref);
    let bt = data.get("BallLastTouch").unwrap_or(&empty);
    let loc = data.get("ImpactLocation").unwrap_or(&empty);
    let scorer = data.get("Scorer").unwrap_or(&empty);
    GoalScoredData {
        goal_speed: f(data, "GoalSpeed"),
        goal_time: f(data, "GoalTime"),
        impact_location: parse_vector3(loc),
        scorer: parse_player_ref(scorer),
        assister,
        ball_last_touch: parse_ball_touch(bt),
    }
}

fn parse_statfeed(data: &Value) -> StatfeedData {
    let empty = Value::Object(Default::default());
    let secondary = data
        .get("SecondaryTarget")
        .filter(|a| a.is_object() && !a.as_object().unwrap().is_empty())
        .map(parse_player_ref);
    let main = data.get("MainTarget").unwrap_or(&empty);
    StatfeedData {
        stat_name: s(data, "EventName"),
        stat_type: s(data, "Type"),
        main_target: parse_player_ref(main),
        secondary_target: secondary,
    }
}

fn parse_match_ended(data: &Value) -> MatchEndedData {
    MatchEndedData {
        winner_team_num: i_or(data, "WinnerTeamNum", -1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_complete_objects_and_keeps_tail() {
        let buf = br#"{"Event":"A"}{"Event":"B","Data":{"x":"}"}}{"Event":"C","#;
        let (objects, rest) = extract_json_objects(buf);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0], br#"{"Event":"A"}"#.to_vec());
        assert_eq!(rest, br#"{"Event":"C","#.to_vec());
    }

    #[test]
    fn parses_goal_scored() {
        let raw = br#"{"Event":"GoalScored","Data":{"MatchGuid":"abc","GoalSpeed":92.5,"Scorer":{"Name":"Player1","TeamNum":1},"BallLastTouch":{"Player":{"Name":"Player1"},"Speed":80.0},"ImpactLocation":{"X":1.0,"Y":2.0,"Z":3.0}}}"#;
        let event = parse_message(raw).unwrap();
        assert_eq!(event.event_type, "GoalScored");
        assert_eq!(event.match_guid.as_deref(), Some("abc"));
        let goal = event.goal_scored().unwrap();
        assert_eq!(goal.scorer.name, "Player1");
        assert_eq!(goal.scorer.team_num, 1);
        assert!((goal.goal_speed - 92.5).abs() < 1e-9);
        assert!((goal.impact_location.z - 3.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_event_becomes_simple() {
        let raw = br#"{"Event":"CountdownBegin","Data":{"MatchGuid":"xyz"}}"#;
        let event = parse_message(raw).unwrap();
        assert_eq!(event.event_type, "CountdownBegin");
        assert!(matches!(event.data, EventData::Simple));
    }
}
