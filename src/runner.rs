use crate::{
    cli,
    cli::PonderMode,
    engine::{self, EngineResult, Score},
    shogi,
    shogi::GameOutcome,
    tc,
    tc::StepResult,
    tournament::{MatchResult, MatchTicket, Tournament, TournamentState},
};
use chrono::Utc;
use log::{info, warn};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Runner {
    engines: Vec<cli::EngineOptions>,
    concurrency: u64,
    adjudication: cli::AdjudicationOptions,
    report_interval: Option<u64>,
}

impl Runner {
    pub fn new(
        engines: Vec<cli::EngineOptions>,
        concurrency: u64,
        adjudication: cli::AdjudicationOptions,
        report_interval: Option<u64>,
    ) -> Runner {
        Runner {
            engines,
            concurrency,
            adjudication,
            report_interval,
        }
    }

    pub fn run(&self, mut tournament: Box<dyn Tournament>) {
        let tournament = tournament.as_mut();

        let (send_ticket, recv_ticket) = crossbeam_channel::bounded(0);
        let (send_result, recv_result) = crossbeam_channel::bounded(0);

        let mut thread_handles = vec![];

        for i in 0..self.concurrency {
            let recv_ticket = recv_ticket.clone();
            let send_result = send_result.clone();
            let engines = self.engines.clone();
            let adjudication = self.adjudication.clone();
            thread_handles.push(thread::spawn(move || {
                runner_thread_main(engines, adjudication, i, recv_ticket, send_result);
            }));
        }

        let mut state = TournamentState::Continue;
        let mut ticket = None;
        let mut match_count = 0;

        let mut match_complete = |tournament: &mut dyn Tournament, result: MatchResult| {
            let state = tournament.match_complete(result);

            match_count += 1;
            if let Some(report_interval) = self.report_interval
                && match_count % report_interval == 0
            {
                println!("--------------------------------------------------------------");
                tournament.print_interval_report();
                println!("--------------------------------------------------------------");
            }

            state
        };

        while state != TournamentState::Stop {
            if ticket.is_none() {
                ticket = tournament.next();
            }
            match ticket {
                None => {
                    crossbeam_channel::select! {
                        recv(recv_result) -> result => state = match_complete(tournament, result.unwrap()),
                    }
                }
                Some(ref t) => {
                    crossbeam_channel::select! {
                        recv(recv_result) -> result => state = match_complete(tournament, result.unwrap()),
                        send(send_ticket, Some(t.clone())) -> result => {
                            assert!(result.is_ok());
                            tournament.match_started(t.clone());
                            ticket = None;
                        }
                    }
                }
            }
        }

        for _ in 0..self.concurrency {
            send_ticket.send(None).unwrap();
        }

        while let Some(h) = thread_handles.pop() {
            h.join().expect("could not join thread");
        }

        tournament.tournament_complete();
    }
}

fn runner_thread_main(
    engine_options: Vec<cli::EngineOptions>,
    adjudication: cli::AdjudicationOptions,
    thread_index: u64,
    recv: crossbeam_channel::Receiver<Option<MatchTicket>>,
    send: crossbeam_channel::Sender<MatchResult>,
) {
    let mut engines: Vec<_> = engine_options
        .iter()
        .map(|o| o.builder.init().unwrap())
        .collect();

    while let Some(ticket) = recv.recv().unwrap() {
        assert!(ticket.engines[0] != ticket.engines[1]);
        info!("Thread {thread_index} received ticket: {:?}", &ticket);

        let result = run_match(&engine_options, &adjudication, &mut engines, &ticket).unwrap();

        info!("Thread {thread_index} sending result: {:?}", &result);
        send.send(result).unwrap();
    }
}

fn do_adjudication(
    stm: shogi::Color,
    adjudication: &cli::AdjudicationOptions,
    match_result: &mut MatchResult,
) {
    if match_result.outcome.is_determined() {
        return;
    }

    if let Some(max_moves) = adjudication.max_moves
        && match_result.moves.len() as u64 >= max_moves
    {
        match_result.outcome = GameOutcome::DrawByMoveLimit;
    }

    if let Some(ref draw) = adjudication.draw
        && match_result.moves.len() >= draw.move_number
        && match_result
            .moves
            .iter()
            .rev()
            .take_while(|m| match m.score {
                Score::Cp(cp) => cp.abs() <= draw.score,
                _ => false,
            })
            .count()
            >= draw.move_count
    {
        match_result.outcome = GameOutcome::DrawByAdjudication;
    }

    if let Some(ref resign) = adjudication.resign
        && !resign.two_sided
        && match_result
            .moves
            .iter()
            .rev()
            .filter(|m| m.stm == Some(stm))
            .take_while(|m| match m.score {
                Score::None => false,
                Score::Cp(cp) => cp <= -resign.score,
                Score::Mate(ply) => ply < 0,
            })
            .count()
            >= resign.move_count
    {
        assert!(Some(stm) == match_result.moves.last().and_then(|m| m.stm));
        match_result.outcome = GameOutcome::WinByAdjudication(!stm);
    }

    if let Some(ref resign) = adjudication.resign
        && resign.two_sided
        && match_result
            .moves
            .iter()
            .rev()
            .take_while(|m| match m.score {
                Score::None => false,
                Score::Cp(cp) => {
                    if Some(stm) == m.stm {
                        cp <= -resign.score
                    } else {
                        cp >= resign.score
                    }
                }
                Score::Mate(ply) => {
                    if Some(stm) == m.stm {
                        ply < 0
                    } else {
                        ply > 0
                    }
                }
            })
            .count()
            >= resign.move_count
    {
        assert!(Some(stm) == match_result.moves.last().and_then(|m| m.stm));
        match_result.outcome = GameOutcome::WinByAdjudication(!stm);
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct PonderState {
    expected_move: shogi::Move,
}

trait MatchEngine {
    fn name(&self) -> &str;
    fn restart(&mut self) -> Result<(), std::io::Error>;
    fn isready(&mut self) -> Result<(), std::io::Error>;
    fn usinewgame(&mut self) -> Result<(), std::io::Error>;
    fn position(&mut self, game: &shogi::Game) -> Result<(), std::io::Error>;
    fn write_line(&mut self, line: &str) -> Result<(), std::io::Error>;
    fn flush(&mut self) -> Result<(), std::io::Error>;
    fn wait_for_bestmove(
        &mut self,
        stm: shogi::Color,
        timeout: Option<Duration>,
    ) -> EngineResult<engine::MoveRecord>;
}

impl MatchEngine for engine::Engine {
    fn name(&self) -> &str {
        engine::Engine::name(self)
    }

    fn restart(&mut self) -> Result<(), std::io::Error> {
        engine::Engine::restart(self)
    }

    fn isready(&mut self) -> Result<(), std::io::Error> {
        engine::Engine::isready(self)
    }

    fn usinewgame(&mut self) -> Result<(), std::io::Error> {
        engine::Engine::usinewgame(self)
    }

    fn position(&mut self, game: &shogi::Game) -> Result<(), std::io::Error> {
        engine::Engine::position(self, game)
    }

    fn write_line(&mut self, line: &str) -> Result<(), std::io::Error> {
        engine::Engine::write_line(self, line)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        engine::Engine::flush(self)
    }

    fn wait_for_bestmove(
        &mut self,
        stm: shogi::Color,
        timeout: Option<Duration>,
    ) -> EngineResult<engine::MoveRecord> {
        engine::Engine::wait_for_bestmove(self, stm, timeout)
    }
}

fn command_with_arguments(command: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        command.to_string()
    } else {
        format!("{command} {arguments}")
    }
}

fn ponder_go_command(mode: PonderMode, time_arguments: &str) -> Option<String> {
    match mode {
        PonderMode::Off => None,
        PonderMode::Standard => Some(command_with_arguments("go ponder", time_arguments)),
        PonderMode::Early => Some("go ponder".to_string()),
    }
}

fn ponder_hit_command(mode: PonderMode, time_arguments: &str) -> String {
    match mode {
        PonderMode::Early => command_with_arguments("ponderhit", time_arguments),
        PonderMode::Off | PonderMode::Standard => "ponderhit".to_string(),
    }
}

fn stop_ponder<E: MatchEngine>(
    engine: &mut E,
    stm: shogi::Color,
    timeout: Duration,
) -> EngineResult<engine::MoveRecord> {
    if let Err(err) = engine.write_line("stop").and_then(|_| engine.flush()) {
        return EngineResult::Err(err);
    }
    engine.wait_for_bestmove(stm, Some(timeout))
}

fn stop_timeout(bestmove_timeout: Option<Duration>) -> Duration {
    let protocol_timeout = Duration::from_secs(5);
    bestmove_timeout.map_or(protocol_timeout, |timeout| timeout.min(protocol_timeout))
}

fn cleanup_active_ponders<E: MatchEngine>(
    engines: &mut [E],
    ticket: &MatchTicket,
    ponder_states: &mut [Option<PonderState>; 2],
) -> Result<(), std::io::Error> {
    for (color_index, ponder_state) in ponder_states.iter_mut().enumerate() {
        if ponder_state.take().is_none() {
            continue;
        }

        let engine_index = ticket.engines[color_index];
        let stm = if color_index == shogi::Color::Sente.to_index() {
            shogi::Color::Sente
        } else {
            shogi::Color::Gote
        };
        match stop_ponder(&mut engines[engine_index], stm, Duration::from_secs(5)) {
            EngineResult::Ok(_) => {}
            EngineResult::Err(err) => {
                warn!(
                    "Failed to stop ponder for {}: {err}; restarting engine",
                    engines[engine_index].name()
                );
                engines[engine_index].restart()?;
            }
            EngineResult::Timeout => {
                warn!(
                    "Timed out stopping ponder for {}; restarting engine",
                    engines[engine_index].name()
                );
                engines[engine_index].restart()?;
            }
            EngineResult::Disconnected => {
                warn!(
                    "Engine {} disconnected while stopping ponder; restarting engine",
                    engines[engine_index].name()
                );
                engines[engine_index].restart()?;
            }
        }
    }
    Ok(())
}

fn recover_after_search_error<E: MatchEngine>(
    engines: &mut [E],
    ticket: &MatchTicket,
    ponder_states: &mut [Option<PonderState>; 2],
    current_engine_index: usize,
) -> Result<(), std::io::Error> {
    let cleanup_result = cleanup_active_ponders(engines, ticket, ponder_states);
    let restart_result = engines[current_engine_index].restart();
    cleanup_result?;
    restart_result
}

fn finish_match<E: MatchEngine>(
    match_result: MatchResult,
    engines: &mut [E],
    ticket: &MatchTicket,
    ponder_states: &mut [Option<PonderState>; 2],
    restart_engine_index: Option<usize>,
) -> Result<MatchResult, std::io::Error> {
    let cleanup_result = cleanup_active_ponders(engines, ticket, ponder_states);
    let restart_result = restart_engine_index
        .map(|engine_index| engines[engine_index].restart())
        .transpose();
    cleanup_result?;
    restart_result?;
    Ok(match_result)
}

fn start_ponder<E: MatchEngine>(
    engine: &mut E,
    mode: PonderMode,
    game: &shogi::Game,
    expected_move: Option<shogi::Move>,
    mover: shogi::Color,
    sente_time: &tc::EngineTime,
    gote_time: &tc::EngineTime,
) -> Result<Option<PonderState>, std::io::Error> {
    let Some(expected_move) = expected_move else {
        return Ok(None);
    };
    let Some(command) = ponder_go_command(mode, &tc::to_usi_string(mover, sente_time, gote_time))
    else {
        return Ok(None);
    };

    let mut ponder_game = game.clone();
    if ponder_game.do_move(expected_move).is_determined() {
        return Ok(None);
    }
    debug_assert_eq!(ponder_game.stm(), mover);

    engine.position(&ponder_game)?;
    engine.write_line(&command)?;
    engine.flush()?;
    Ok(Some(PonderState { expected_move }))
}

fn run_match<E: MatchEngine>(
    engine_options: &[cli::EngineOptions],
    adjudication: &cli::AdjudicationOptions,
    engines: &mut [E],
    ticket: &MatchTicket,
) -> Result<MatchResult, std::io::Error> {
    let mut match_result = MatchResult {
        ticket: ticket.clone(),
        game_start: Utc::now(),
        outcome: shogi::GameOutcome::Undetermined,
        moves: vec![],
    };

    let mut engine_time = [
        tc::EngineTime::new(
            engine_options[ticket.engines[0]].time_control,
            engine_options[ticket.engines[0]].time_margin,
        ),
        tc::EngineTime::new(
            engine_options[ticket.engines[1]].time_control,
            engine_options[ticket.engines[1]].time_margin,
        ),
    ];

    for i in 0..2 {
        if engine_options[ticket.engines[i]].restart {
            engines[ticket.engines[i]].restart()?;
        }
        engines[ticket.engines[i]].isready()?;
        engines[ticket.engines[i]].usinewgame()?;
    }

    let mut game = shogi::Game::new(ticket.opening);
    let mut ponder_states: [Option<PonderState>; 2] = [None, None];
    let mut last_move = None;
    loop {
        let stm = game.stm();
        let color_index = stm.to_index();
        let engine_index = ticket.engines[color_index];
        let ponder_mode = engine_options[engine_index].ponder_mode;
        let bestmove_timeout = engine_time[stm.to_index()].bestmove_timeout();
        let time_arguments = tc::to_usi_string(stm, &engine_time[0], &engine_time[1]);

        // TODO: Improve time measurement here
        let now = Instant::now();
        let mut search_started_by_ponderhit = false;
        let mut restart_current_on_finish = false;
        if let Some(ponder_state) = ponder_states[color_index].take() {
            if last_move == Some(ponder_state.expected_move) {
                let command = ponder_hit_command(ponder_mode, &time_arguments);
                let current_engine = &mut engines[engine_index];
                if let Err(err) = current_engine
                    .write_line(&command)
                    .and_then(|_| current_engine.flush())
                {
                    recover_after_search_error(engines, ticket, &mut ponder_states, engine_index)?;
                    return Err(err);
                }
                search_started_by_ponderhit = true;
            } else {
                match stop_ponder(
                    &mut engines[engine_index],
                    stm,
                    stop_timeout(bestmove_timeout),
                ) {
                    EngineResult::Ok(_) => {}
                    EngineResult::Err(err) => {
                        recover_after_search_error(
                            engines,
                            ticket,
                            &mut ponder_states,
                            engine_index,
                        )?;
                        return Err(err);
                    }
                    EngineResult::Timeout => {
                        match_result.outcome = GameOutcome::LossByClock(stm);
                        restart_current_on_finish = true;
                    }
                    EngineResult::Disconnected => {
                        match_result.outcome = GameOutcome::LossByDisconnection(stm);
                        restart_current_on_finish = true;
                    }
                }
            }
        }

        if match_result.outcome.is_determined() {
            return finish_match(
                match_result,
                engines,
                ticket,
                &mut ponder_states,
                restart_current_on_finish.then_some(engine_index),
            );
        }

        if !search_started_by_ponderhit {
            let current_engine = &mut engines[engine_index];
            let send_result = current_engine.position(&game).and_then(|_| {
                current_engine.write_line(&format!("go {time_arguments}"))?;
                current_engine.flush()
            });
            if let Err(err) = send_result {
                recover_after_search_error(engines, ticket, &mut ponder_states, engine_index)?;
                return Err(err);
            }
        }

        match engines[engine_index].wait_for_bestmove(stm, bestmove_timeout) {
            EngineResult::Err(err) => {
                recover_after_search_error(engines, ticket, &mut ponder_states, engine_index)?;
                return Err(err);
            }

            EngineResult::Ok(mut move_record) => {
                let duration = Instant::now() - now;
                let time_outcome = engine_time[stm.to_index()].step(duration);
                move_record.measured_time = duration;
                move_record.time_left = engine_time[stm.to_index()].remaining();

                let m = move_record.m;
                let ponder_move = move_record.ponder;
                match_result.moves.push(move_record);
                match_result.outcome = game.do_move(m);
                last_move = Some(m);

                if time_outcome == StepResult::TimeElapsed {
                    match_result.outcome = GameOutcome::LossByClock(stm);
                }

                do_adjudication(stm, adjudication, &mut match_result);

                if !match_result.outcome.is_determined() {
                    ponder_states[color_index] = match start_ponder(
                        &mut engines[engine_index],
                        ponder_mode,
                        &game,
                        ponder_move,
                        stm,
                        &engine_time[0],
                        &engine_time[1],
                    ) {
                        Ok(state) => state,
                        Err(err) => {
                            recover_after_search_error(
                                engines,
                                ticket,
                                &mut ponder_states,
                                engine_index,
                            )?;
                            return Err(err);
                        }
                    };
                }
            }

            EngineResult::Timeout => {
                match_result.outcome = GameOutcome::LossByClock(stm);
                restart_current_on_finish = true;
            }

            EngineResult::Disconnected => {
                match_result.outcome = GameOutcome::LossByDisconnection(stm);
                restart_current_on_finish = true;
            }
        };

        if match_result.outcome.is_determined() {
            return finish_match(
                match_result,
                engines,
                ticket,
                &mut ponder_states,
                restart_current_on_finish.then_some(engine_index),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shogi::Color;
    use std::collections::VecDeque;

    struct MockEngine {
        name: String,
        commands: Vec<String>,
        results: VecDeque<EngineResult<engine::MoveRecord>>,
    }

    impl MockEngine {
        fn new(name: &str, results: Vec<EngineResult<engine::MoveRecord>>) -> Self {
            Self {
                name: name.to_string(),
                commands: vec![],
                results: results.into(),
            }
        }
    }

    impl MatchEngine for MockEngine {
        fn name(&self) -> &str {
            &self.name
        }

        fn restart(&mut self) -> Result<(), std::io::Error> {
            self.commands.push("<restart>".to_string());
            Ok(())
        }

        fn isready(&mut self) -> Result<(), std::io::Error> {
            self.commands.push("isready".to_string());
            Ok(())
        }

        fn usinewgame(&mut self) -> Result<(), std::io::Error> {
            self.commands.push("usinewgame".to_string());
            Ok(())
        }

        fn position(&mut self, game: &shogi::Game) -> Result<(), std::io::Error> {
            self.commands
                .push(format!("position {}", game.usi_string()));
            Ok(())
        }

        fn write_line(&mut self, line: &str) -> Result<(), std::io::Error> {
            self.commands.push(line.to_string());
            Ok(())
        }

        fn flush(&mut self) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn wait_for_bestmove(
            &mut self,
            _stm: shogi::Color,
            _timeout: Option<Duration>,
        ) -> EngineResult<engine::MoveRecord> {
            self.results.pop_front().unwrap_or_else(|| {
                EngineResult::Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("No queued bestmove for {}", self.name),
                ))
            })
        }
    }

    fn move_result(bestmove: &str, ponder: Option<&str>) -> EngineResult<engine::MoveRecord> {
        EngineResult::Ok(engine::MoveRecord {
            m: shogi::Move::parse(bestmove).unwrap(),
            mstr: bestmove.to_string(),
            ponder: ponder.and_then(shogi::Move::parse),
            ..engine::MoveRecord::default()
        })
    }

    fn engine_options(ponder_mode: PonderMode) -> cli::EngineOptions {
        cli::EngineOptions {
            time_control: tc::TimeControl::Fischer {
                base: Duration::from_secs(60),
                increment: Duration::from_secs(2),
            },
            ponder_mode,
            ..cli::EngineOptions::default()
        }
    }

    fn match_ticket() -> MatchTicket {
        MatchTicket {
            id: 0,
            engines: [0, 1],
            opening: shogi::Position::default(),
        }
    }

    fn adjudication_with_max_moves(max_moves: u64) -> cli::AdjudicationOptions {
        cli::AdjudicationOptions {
            max_moves: Some(max_moves),
            draw: None,
            resign: None,
        }
    }

    fn command_index(commands: &[String], expected: &str) -> usize {
        commands
            .iter()
            .position(|command| command == expected)
            .unwrap_or_else(|| panic!("Missing command {expected:?} in {commands:#?}"))
    }

    #[test]
    fn builds_standard_ponder_commands() {
        let clocks = "btime 60000 wtime 30000 binc 2000 winc 2000";

        assert_eq!(
            ponder_go_command(PonderMode::Standard, clocks).as_deref(),
            Some("go ponder btime 60000 wtime 30000 binc 2000 winc 2000")
        );
        assert_eq!(
            ponder_hit_command(PonderMode::Standard, clocks),
            "ponderhit"
        );
    }

    #[test]
    fn builds_early_ponder_commands() {
        let clocks = "btime 60000 wtime 30000 byoyomi 5000";

        assert_eq!(
            ponder_go_command(PonderMode::Early, clocks).as_deref(),
            Some("go ponder")
        );
        assert_eq!(
            ponder_hit_command(PonderMode::Early, clocks),
            "ponderhit btime 60000 wtime 30000 byoyomi 5000"
        );
    }

    #[test]
    fn disables_ponder_commands_by_default() {
        assert_eq!(ponder_go_command(PonderMode::Off, "btime 1000"), None);
    }

    #[test]
    fn runs_standard_miss_and_early_hit_with_protocol_synchronization() {
        let options = [
            engine_options(PonderMode::Standard),
            engine_options(PonderMode::Early),
        ];
        let mut engines = [
            MockEngine::new(
                "standard",
                vec![
                    move_result("7g7f", Some("8c8d")),
                    move_result("2g2f", None),
                    move_result("2g2f", Some("8c8d")),
                    move_result("2f2e", None),
                ],
            ),
            MockEngine::new(
                "early",
                vec![
                    move_result("3c3d", Some("2g2f")),
                    move_result("8c8d", Some("2f2e")),
                ],
            ),
        ];

        let result = run_match(
            &options,
            &adjudication_with_max_moves(4),
            &mut engines,
            &match_ticket(),
        )
        .unwrap();

        assert_eq!(result.outcome, GameOutcome::DrawByMoveLimit);

        let standard_commands = &engines[0].commands;
        let first_ponder = standard_commands
            .iter()
            .position(|command| command.starts_with("go ponder btime "))
            .expect("standard ponder was not started with clocks");
        let miss_stop = standard_commands[first_ponder + 1..]
            .iter()
            .position(|command| command == "stop")
            .map(|index| index + first_ponder + 1)
            .expect("standard ponder miss did not send stop");
        let actual_position = standard_commands[miss_stop + 1..]
            .iter()
            .position(|command| command.ends_with("moves 7g7f 3c3d"))
            .map(|index| index + miss_stop + 1)
            .expect("actual position was not sent after draining ponder miss");
        assert!(first_ponder < miss_stop && miss_stop < actual_position);

        let early_commands = &engines[1].commands;
        assert!(early_commands.iter().any(|command| command == "go ponder"));
        assert!(early_commands.iter().any(|command| {
            command.starts_with("ponderhit wtime ")
                && command.contains(" winc 2000 btime ")
                && command.ends_with(" binc 2000")
        }));
    }

    #[test]
    fn runs_standard_ponder_hit_with_bare_ponderhit() {
        let options = [
            engine_options(PonderMode::Standard),
            engine_options(PonderMode::Early),
        ];
        let mut engines = [
            MockEngine::new(
                "standard",
                vec![
                    move_result("7g7f", Some("3c3d")),
                    move_result("2g2f", Some("8c8d")),
                ],
            ),
            MockEngine::new(
                "early",
                vec![move_result("3c3d", Some("2g2f")), move_result("8c8d", None)],
            ),
        ];

        let result = run_match(
            &options,
            &adjudication_with_max_moves(3),
            &mut engines,
            &match_ticket(),
        )
        .unwrap();

        assert_eq!(result.outcome, GameOutcome::DrawByMoveLimit);
        let commands = &engines[0].commands;
        let ponder_go = commands
            .iter()
            .position(|command| command.starts_with("go ponder btime "))
            .expect("standard ponder was not started");
        let ponderhit = command_index(commands, "ponderhit");
        assert!(ponder_go < ponderhit);
        assert!(
            !commands
                .iter()
                .any(|command| command.starts_with("ponderhit "))
        );
    }

    #[test]
    fn keeps_the_existing_non_ponder_search_path() {
        let options = [
            engine_options(PonderMode::Off),
            engine_options(PonderMode::Off),
        ];
        let mut engines = [
            MockEngine::new("first", vec![move_result("7g7f", Some("3c3d"))]),
            MockEngine::new("second", vec![move_result("3c3d", Some("2g2f"))]),
        ];

        let result = run_match(
            &options,
            &adjudication_with_max_moves(2),
            &mut engines,
            &match_ticket(),
        )
        .unwrap();

        assert_eq!(result.outcome, GameOutcome::DrawByMoveLimit);
        for engine in &engines {
            assert!(!engine.commands.iter().any(|command| {
                command == "stop"
                    || command.starts_with("go ponder")
                    || command.starts_with("ponderhit")
            }));
        }
    }

    fn new_mr() -> MatchResult {
        MatchResult {
            ticket: MatchTicket {
                id: 0,
                engines: [0, 1],
                opening: shogi::Position::default(),
            },
            game_start: Utc::now(),
            outcome: GameOutcome::Undetermined,
            moves: vec![],
        }
    }

    fn append(mr: &mut MatchResult, stm: Color, score: Score) {
        mr.moves.push(engine::MoveRecord {
            stm: Some(stm),
            score,
            ..engine::MoveRecord::default()
        });
    }

    #[test]
    fn test_resign_1() {
        let mut mr = new_mr();
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1000));
        append(&mut mr, Color::Gote, Score::Cp(-1000));
        append(&mut mr, Color::Sente, Score::Cp(1000));
        append(&mut mr, Color::Gote, Score::Cp(-1000));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: false,
                    move_count: 1,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::WinByAdjudication(Color::Sente));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: false,
                    move_count: 2,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::WinByAdjudication(Color::Sente));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: false,
                    move_count: 3,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::Undetermined);

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: true,
                    move_count: 2,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::WinByAdjudication(Color::Sente));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: true,
                    move_count: 4,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::WinByAdjudication(Color::Sente));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: true,
                    move_count: 6,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::Undetermined);
    }

    #[test]
    fn test_resign_2() {
        let mut mr = new_mr();
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(-1000));
        append(&mut mr, Color::Gote, Score::Cp(-1000));
        append(&mut mr, Color::Sente, Score::Cp(-1000));
        append(&mut mr, Color::Gote, Score::Cp(-1000));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: false,
                    move_count: 2,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::WinByAdjudication(Color::Sente));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: true,
                    move_count: 2,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::Undetermined);

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: true,
                    move_count: 4,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::Undetermined);
    }

    #[test]
    fn test_resign_3() {
        let mut mr = new_mr();
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(-1000));
        append(&mut mr, Color::Gote, Score::Mate(6));
        append(&mut mr, Color::Sente, Score::Cp(-1000));
        append(&mut mr, Color::Gote, Score::Mate(4));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: false,
                    move_count: 2,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::Undetermined);
    }

    #[test]
    fn test_resign_4() {
        let mut mr = new_mr();
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1));
        append(&mut mr, Color::Gote, Score::Cp(-1));
        append(&mut mr, Color::Sente, Score::Cp(1000));
        append(&mut mr, Color::Gote, Score::Mate(-6));
        append(&mut mr, Color::Sente, Score::Cp(1000));
        append(&mut mr, Color::Gote, Score::Mate(-4));

        mr.outcome = GameOutcome::Undetermined;
        do_adjudication(
            Color::Gote,
            &cli::AdjudicationOptions {
                max_moves: None,
                draw: None,
                resign: Some(cli::ResignAdjudicationOptions {
                    two_sided: false,
                    move_count: 2,
                    score: 200,
                }),
            },
            &mut mr,
        );
        assert!(mr.outcome == GameOutcome::WinByAdjudication(Color::Sente));
    }
}
