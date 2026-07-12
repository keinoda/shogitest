use std::{fmt, time::Duration};

use crate::engine;
use crate::tc;

#[derive(Debug, Clone)]
pub struct MetaDataOptions {
    pub event_name: String,
    pub site_name: String,
}

#[derive(Debug, Clone)]
pub struct BookOptions {
    pub file: String,
    pub random_order: bool,
    pub start_index: usize,
}

impl Default for BookOptions {
    fn default() -> Self {
        BookOptions {
            file: String::from("<none>"),
            random_order: false,
            start_index: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdjudicationOptions {
    pub max_moves: Option<u64>,
    pub draw: Option<DrawAdjudicationOptions>,
    pub resign: Option<ResignAdjudicationOptions>,
}

impl Default for AdjudicationOptions {
    fn default() -> Self {
        AdjudicationOptions {
            max_moves: Some(512),
            draw: None,
            resign: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DrawAdjudicationOptions {
    pub move_number: usize,
    pub move_count: usize,
    pub score: i32,
}

impl Default for DrawAdjudicationOptions {
    fn default() -> Self {
        DrawAdjudicationOptions {
            move_number: 0,
            move_count: 1,
            score: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResignAdjudicationOptions {
    pub move_count: usize,
    pub score: i32,
    pub two_sided: bool,
}

impl Default for ResignAdjudicationOptions {
    fn default() -> Self {
        ResignAdjudicationOptions {
            move_count: 1,
            score: 0,
            two_sided: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SprtOptions {
    pub nelo0: f64,
    pub nelo1: f64,
    pub alpha: f64,
    pub beta: f64,
}

impl Default for SprtOptions {
    fn default() -> Self {
        SprtOptions {
            nelo0: 0.0,
            nelo1: 0.0,
            alpha: 0.0,
            beta: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CliOptions {
    pub engines: Vec<EngineOptions>,
    pub book: Option<BookOptions>,
    pub games: Option<u64>,
    pub rounds: u64,
    pub concurrency: u64,
    pub rand_seed: Option<u64>,
    pub meta: MetaDataOptions,
    pub pgn: Option<PgnOutOptions>,
    pub adjudication: AdjudicationOptions,
    pub report_interval: Option<u64>,
    pub sprt: Option<SprtOptions>,
}

impl CliOptions {
    pub fn engine_names(&self) -> Vec<String> {
        self.engines
            .iter()
            .map(|e| e.builder.init().unwrap().name().to_string())
            .collect()
    }
}

impl Default for CliOptions {
    fn default() -> Self {
        CliOptions {
            engines: vec![],
            book: None,
            games: None,
            rounds: 2,
            concurrency: 1,
            rand_seed: None,
            meta: MetaDataOptions {
                event_name: String::from("?"),
                site_name: String::from("?"),
            },
            pgn: None,
            adjudication: AdjudicationOptions::default(),
            report_interval: Some(10),
            sprt: None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum PonderMode {
    #[default]
    Off,
    Standard,
    Early,
}

impl PonderMode {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "off" | "false" => Some(Self::Off),
            "on" | "true" | "standard" => Some(Self::Standard),
            "early" => Some(Self::Early),
            _ => None,
        }
    }
}

impl fmt::Display for PonderMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Standard => write!(f, "standard"),
            Self::Early => write!(f, "early"),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct EngineOptions {
    pub builder: engine::EngineBuilder,
    pub time_control: tc::TimeControl,
    pub time_margin: Duration,
    pub restart: bool,
    pub ponder_mode: PonderMode,
}

#[derive(Debug, Clone)]
pub struct PgnOutOptions {
    pub file: String,
    pub track_nodes: bool,
    pub track_seldepth: bool,
    pub track_nps: bool,
    pub track_hashfull: bool,
    pub track_timeleft: bool,
    pub track_latency: bool,
}

impl Default for PgnOutOptions {
    fn default() -> Self {
        PgnOutOptions {
            file: String::default(),
            track_nodes: true,
            track_seldepth: true,
            track_nps: false,
            track_hashfull: false,
            track_timeleft: false,
            track_latency: false,
        }
    }
}

fn parse_engine_option(engine: &mut EngineOptions, name: &str, value: &str) -> bool {
    match name {
        "name" => {
            engine.builder.name = Some(String::from(value));
        }
        "dir" => {
            engine.builder.dir = String::from(value);
        }
        "cmd" => {
            engine.builder.cmd = String::from(value);
        }
        "tc" => {
            if engine.time_control != tc::TimeControl::None {
                eprintln!("Warning; Specifying multiple time controls!");
            }
            if let Some(tc) = tc::TimeControl::parse(value) {
                engine.time_control = tc;
            } else {
                eprintln!("Invalid time control specification {value}");
                return false;
            }
        }
        "st" => {
            if engine.time_control != tc::TimeControl::None {
                eprintln!("Warning; Specifying multiple time controls!");
            }
            match value.parse::<u64>() {
                Ok(value) => {
                    engine.time_control = tc::TimeControl::MoveTime(Duration::from_millis(value));
                }
                Err(_) => {
                    eprintln!("Expected number for st option");
                    return false;
                }
            }
        }
        "nodes" => {
            if engine.time_control != tc::TimeControl::None {
                eprintln!("Warning; Specifying multiple time controls!");
            }
            match value.parse::<u64>() {
                Ok(value) => engine.time_control = tc::TimeControl::Nodes(value),
                Err(_) => {
                    eprintln!("Expected number for st option");
                    return false;
                }
            }
        }
        "timemargin" => match value.parse::<u64>() {
            Ok(value) => engine.time_margin = Duration::from_millis(value),
            Err(_) => {
                eprintln!("Expected number for timemargin option");
                return false;
            }
        },
        "restart" => match value {
            "on" => engine.restart = true,
            "off" => engine.restart = false,
            _ => {
                eprintln!("Invalid value {value} for engine restart option");
                return false;
            }
        },
        "ponder" => match PonderMode::parse(value) {
            Some(mode) => engine.ponder_mode = mode,
            None => {
                eprintln!(
                    "Invalid value {value} for engine ponder option (expected off, standard, or early)"
                );
                return false;
            }
        },
        "proto" => match value {
            "usi" => {}
            _ => {
                eprintln!("Invalid value {value} for engine proto option");
                return false;
            }
        },
        name if let Some(optionname) = name.strip_prefix("option.") => {
            engine
                .builder
                .usi_options
                .push((optionname.to_string(), value.to_string()));
        }
        _ => {
            eprintln!("Invalid engine option: {name}={value}");
            return false;
        }
    }
    true
}

fn apply_ponder_engine_options(engine: &mut EngineOptions) {
    if engine.ponder_mode == PonderMode::Off {
        return;
    }

    let ponder_is_enabled = engine
        .builder
        .get_usi_option_value("USI_Ponder")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    if !ponder_is_enabled {
        engine
            .builder
            .usi_options
            .push(("USI_Ponder".to_string(), "true".to_string()));
    }
}

fn has_early_ponder_clock(engine: &EngineOptions) -> bool {
    matches!(
        engine.time_control,
        tc::TimeControl::MoveTime(_)
            | tc::TimeControl::Byoyomi { .. }
            | tc::TimeControl::Fischer { .. }
    )
}

pub fn parse() -> Option<CliOptions> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut options = CliOptions::default();
    let mut each_options = Vec::<(String, String)>::new();

    let mut it = args.iter().peekable();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "-version" | "--version" => {
                println!("Shogitest version 0.1.3");
                return None;
            }

            "-variant" => {
                let Some(value) = it.next() else {
                    eprintln!("No value for -variant");
                    return None;
                };
                if value != "standard" {
                    eprintln!("Unrecognised value of -variant");
                    return None;
                }
            }

            "-event" => {
                let Some(value) = it.next() else {
                    eprintln!("No value for -event");
                    return None;
                };
                options.meta.event_name = value.to_string();
            }

            "-site" => {
                let Some(value) = it.next() else {
                    eprintln!("No value for -site");
                    return None;
                };
                options.meta.site_name = value.to_string();
            }

            "-engine" => {
                let mut engine = EngineOptions::default();
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    if !parse_engine_option(&mut engine, name, value) {
                        return None;
                    }
                }
                options.engines.push(engine);
            }

            "-each" => {
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    each_options.push((name.to_string(), value.to_string()));
                }
            }

            "-openings" => {
                if options.book.is_some() {
                    eprintln!("Duplicate -openings flag");
                    return None;
                }

                let mut book = BookOptions::default();
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    match name {
                        "file" => {
                            book.file = String::from(value);
                        }
                        "order" => {
                            book.random_order = value == "random";
                        }
                        "start" => {
                            if let Ok(value) = value.parse::<usize>() {
                                if value == 0 {
                                    eprintln!(
                                        "invalid openings start index {value} (must be bigger than zero)"
                                    );
                                    return None;
                                }
                                book.start_index = value;
                            } else {
                                eprintln!(
                                    "invalid openings start index {value} (must be unsigned integer)"
                                );
                                return None;
                            }
                        }
                        "format" => match value {
                            "epd" => {}
                            _ => {
                                eprintln!("Invalid value {value} for openings format option");
                                return None;
                            }
                        },
                        _ => {
                            eprintln!("Unrecognised openings option {name}={value}");
                            return None;
                        }
                    }
                }
                options.book = Some(book);
            }

            "-concurrency" => {
                let Some(option) = it.next() else { break };
                if let Ok(option) = option.parse::<u64>() {
                    if option == 0 {
                        eprintln!("invalid concurrency value {option} (must be bigger than zero)");
                        return None;
                    }
                    options.concurrency = option;
                } else {
                    eprintln!("invalid concurrency value {option} (must be unsigned integer)");
                    return None;
                }
            }

            "-srand" => {
                let Some(option) = it.next() else { break };
                if let Ok(option) = option.parse::<u64>() {
                    options.rand_seed = Some(option);
                } else {
                    eprintln!("invalid random seed {option} (must be unsigned integer)");
                    return None;
                }
            }

            "-games" => {
                let Some(option) = it.next() else { break };
                if let Ok(option) = option.parse::<u64>() {
                    if option == 0 {
                        eprintln!("invalid games value {option} (must be bigger than zero)");
                        return None;
                    }
                    options.games = Some(option);
                } else {
                    eprintln!("invalid games value {option} (must be unsigned integer)");
                    return None;
                }
            }

            "-rounds" => {
                let Some(option) = it.next() else { break };
                if let Ok(option) = option.parse::<u64>() {
                    if option == 0 {
                        eprintln!("invalid rounds value {option} (must be bigger than zero)");
                        return None;
                    }
                    if option % 2 != 0 {
                        eprintln!("odd value for rounds {option}! expected an even value.");
                        return None;
                    }
                    if option > 2 {
                        eprintln!(
                            "Warning; There is often no good reason for a round to have more than two games. (Current value: {option})"
                        );
                    }
                    options.rounds = option;
                } else {
                    eprintln!("invalid rounds value {option} (must be unsigned integer)");
                    return None;
                }
            }

            "-repeat" => {
                options.rounds = 2;
            }

            "-pgnout" => {
                let mut pgn_out = PgnOutOptions::default();
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    let value_as_bool = || -> Option<bool> {
                        match value {
                            "true" => Some(true),
                            "false" => Some(false),
                            _ => None,
                        }
                    };

                    match name {
                        "file" => {
                            pgn_out.file = String::from(value);
                        }
                        "nodes" => {
                            pgn_out.track_nodes = value_as_bool()?;
                        }
                        "seldepth" => {
                            pgn_out.track_seldepth = value_as_bool()?;
                        }
                        "nps" => {
                            pgn_out.track_nps = value_as_bool()?;
                        }
                        "hashfull" => {
                            pgn_out.track_hashfull = value_as_bool()?;
                        }
                        "timeleft" => {
                            pgn_out.track_timeleft = value_as_bool()?;
                        }
                        "latency" => {
                            pgn_out.track_latency = value_as_bool()?;
                        }
                        _ => {
                            dbg!(&name);
                            dbg!(&value);
                        }
                    }

                    if pgn_out.file.is_empty() {
                        eprintln!("output file required for -pgnout option");
                        return None;
                    }
                }
                options.pgn = Some(pgn_out);
            }

            "-maxmoves" => {
                let Some(value) = it.next() else { break };
                options.adjudication.max_moves = match value.to_lowercase().as_str() {
                    "inf" | "infinite" => None,
                    _ if let Ok(value) = value.parse::<u64>()
                        && value > 0 =>
                    {
                        Some(value)
                    }
                    _ => {
                        eprintln!(
                            "invalid maxmoves value {value} (must be non-zero unsigned integer)"
                        );
                        return None;
                    }
                };
            }

            "-draw" => {
                let mut draw = DrawAdjudicationOptions::default();
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    match name {
                        "movenumber" => {
                            draw.move_number = match value.parse::<usize>() {
                                Ok(value) => value,
                                _ => {
                                    eprintln!("Invalid movenumber {value} for -draw");
                                    return None;
                                }
                            };
                        }
                        "movecount" => {
                            draw.move_count = match value.parse::<usize>() {
                                Ok(value) if value > 0 => value,
                                _ => {
                                    eprintln!("Invalid movecount {value} for -draw");
                                    return None;
                                }
                            };
                        }
                        "score" => {
                            draw.score = match value.parse::<i32>() {
                                Ok(value) if value >= 0 => value,
                                _ => {
                                    eprintln!("Invalid score {value} for -draw");
                                    return None;
                                }
                            };
                        }
                        _ => {
                            eprintln!("Invalid key {name} for -draw");
                            return None;
                        }
                    }
                }
                options.adjudication.draw = Some(draw);
            }

            "-resign" => {
                let mut resign = ResignAdjudicationOptions::default();
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    match name {
                        "movecount" => {
                            resign.move_count = match value.parse::<usize>() {
                                Ok(value) if value > 0 => value,
                                _ => {
                                    eprintln!("Invalid movecount {value} for -resign");
                                    return None;
                                }
                            };
                        }
                        "score" => {
                            resign.score = match value.parse::<i32>() {
                                Ok(value) if value >= 0 => value,
                                _ => {
                                    eprintln!("Invalid score {value} for -resign");
                                    return None;
                                }
                            };
                        }
                        "twosided" => {
                            resign.two_sided = match value.to_lowercase().as_ref() {
                                "true" => true,
                                "false" => false,
                                _ => {
                                    eprintln!("Invalid boolean {value} for twosided for -resign");
                                    return None;
                                }
                            };
                        }
                        _ => {
                            eprintln!("Invalid key {name} for -resign");
                            return None;
                        }
                    }
                }
                options.adjudication.resign = Some(resign);
            }

            "-ratinginterval" => {
                let Some(option) = it.next() else { break };
                if let Ok(option) = option.parse::<u64>() {
                    options.report_interval = if option == 0 { None } else { Some(option) };
                } else {
                    eprintln!("invalid games value {option} (must be unsigned integer)");
                    return None;
                }
            }

            "-sprt" => {
                let mut sprt = SprtOptions::default();
                while let Some(option) = it.peek()
                    && !option.starts_with("-")
                    && let Some((name, value)) = option.split_once('=')
                {
                    it.next(); // consume token

                    match name {
                        "elo0" => {
                            sprt.nelo0 = match value.parse::<f64>() {
                                Ok(value) => value,
                                _ => {
                                    eprintln!("Invalid elo0 {value} for -sprt");
                                    return None;
                                }
                            };
                        }
                        "elo1" => {
                            sprt.nelo1 = match value.parse::<f64>() {
                                Ok(value) => value,
                                _ => {
                                    eprintln!("Invalid elo1 {value} for -sprt");
                                    return None;
                                }
                            };
                        }
                        "alpha" => {
                            sprt.alpha = match value.parse::<f64>() {
                                Ok(value) => value,
                                _ => {
                                    eprintln!("Invalid alpha {value} for -sprt");
                                    return None;
                                }
                            };
                        }
                        "beta" => {
                            sprt.beta = match value.parse::<f64>() {
                                Ok(value) => value,
                                _ => {
                                    eprintln!("Invalid beta {value} for -sprt");
                                    return None;
                                }
                            };
                        }
                        _ => {
                            eprintln!("Invalid key {name} for -sprt");
                            return None;
                        }
                    }
                }
                options.sprt = Some(sprt);
            }

            "-testEnv" => {
                options.report_interval = None;
            }

            "-recover" => {
                // We always recover on disconnects
            }

            _ => {
                eprintln!("Unrecognised command line flag {flag}");
                return None;
            }
        }
    }

    for (name, value) in each_options {
        for engine in &mut options.engines {
            if !parse_engine_option(engine, &name, &value) {
                return None;
            }
        }
    }

    for engine in &mut options.engines {
        apply_ponder_engine_options(engine);
        if engine.ponder_mode == PonderMode::Early && !has_early_ponder_clock(engine) {
            eprintln!(
                "Early ponder requires a clock-based time control (Fischer, byoyomi, or movetime)"
            );
            return None;
        }
    }

    if options.sprt.is_some() && options.engines.len() != 2 {
        eprintln!("SPRT can only be done on two engines");
        return None;
    }

    Some(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_ponder_modes() {
        let mut engine = EngineOptions::default();

        assert!(parse_engine_option(&mut engine, "ponder", "standard"));
        assert_eq!(engine.ponder_mode, PonderMode::Standard);

        assert!(parse_engine_option(&mut engine, "ponder", "early"));
        assert_eq!(engine.ponder_mode, PonderMode::Early);

        assert!(parse_engine_option(&mut engine, "ponder", "off"));
        assert_eq!(engine.ponder_mode, PonderMode::Off);

        assert!(!parse_engine_option(&mut engine, "ponder", "invalid"));
    }

    #[test]
    fn enabling_ponder_adds_the_usi_option() {
        let mut engine = EngineOptions {
            ponder_mode: PonderMode::Early,
            ..EngineOptions::default()
        };

        apply_ponder_engine_options(&mut engine);

        assert_eq!(
            engine.builder.get_usi_option_value("USI_Ponder"),
            Some("true")
        );
    }

    #[test]
    fn enabling_ponder_overrides_an_explicit_false_usi_option() {
        let mut engine = EngineOptions {
            ponder_mode: PonderMode::Standard,
            ..EngineOptions::default()
        };
        engine
            .builder
            .usi_options
            .push(("USI_Ponder".to_string(), "false".to_string()));

        apply_ponder_engine_options(&mut engine);

        assert_eq!(
            engine.builder.get_usi_option_value("USI_Ponder"),
            Some("true")
        );
    }

    #[test]
    fn early_ponder_requires_clock_arguments_for_ponderhit() {
        let mut engine = EngineOptions {
            ponder_mode: PonderMode::Early,
            ..EngineOptions::default()
        };

        assert!(!has_early_ponder_clock(&engine));
        engine.time_control = tc::TimeControl::Nodes(1000);
        assert!(!has_early_ponder_clock(&engine));
        engine.time_control = tc::TimeControl::MoveTime(Duration::from_secs(1));
        assert!(has_early_ponder_clock(&engine));
    }
}
