#[path = "../common.rs"]
mod common;
use common::*;

use std::{io::{self, Write as _, BufWriter}, fs, path::PathBuf, time::Duration, fmt::Write as _};
use tokio::{net::TcpStream, io::{AsyncWriteExt, AsyncBufReadExt, BufReader}, sync::mpsc};
use crossterm::{event::{Event, KeyCode, KeyEventKind, KeyModifiers, EventStream}, execute, queue, terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, enable_raw_mode, disable_raw_mode}, cursor};
use inquire::{Text, Select, error::InquireError};
use chrono::Local;
use serde::{Deserialize, Serialize};
use futures_util::StreamExt;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Config {
    addr: Option<String>,
    name: Option<String>,
}

struct GameState {
    cursor: (usize, usize),
    board: [[Piece; BOARD_SIZE]; BOARD_SIZE],
    my_piece: Piece,
    turn: Piece,
    room_id: String,
    opp_ready: bool,
    ready: bool,
    winner: Option<String>,
    winner_name: String,
    opponent_name: String,
    my_name: String,
}

fn config_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".gcli");
    if !path.exists() { fs::create_dir_all(&path).ok(); }
    path.push("config.json");
    path
}

fn load_config() -> Config {
    let path = config_path();
    if let Ok(data) = fs::read_to_string(path) { serde_json::from_str(&data).unwrap_or_default() } else { Config::default() }
}

fn save_config(config: &Config) {
    let path = config_path();
    if let Ok(json) = serde_json::to_string(config) { fs::write(path, json).ok(); }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut config = load_config();
    let mut stream: Option<TcpStream> = None;

    'outer: loop {
        // Step 1: Addr
        while config.addr.is_none() {
            match Text::new("Server Addr:").with_default("127.0.0.1:8080").prompt() {
                Ok(v) => {
                    match TcpStream::connect(&v).await {
                        Ok(s) => { config.addr = Some(v); stream = Some(s); }
                        Err(e) => println!("Connection fail: {}", e),
                    }
                }
                Err(InquireError::OperationCanceled) => { show_stealth_mode(config.addr.as_deref()).await?; }
                Err(_) => std::process::exit(0),
            }
        }

        // Step 2: Name
        while config.name.is_none() {
            match Text::new("Your Name (or type 'back' to change addr):").prompt() {
                Ok(v) if v == "back" => { config.addr = None; config.name = None; stream = None; continue 'outer; }
                Ok(v) => { config.name = Some(v); }
                Err(InquireError::OperationCanceled) => { show_stealth_mode(config.addr.as_deref()).await?; }
                Err(_) => std::process::exit(0),
            }
        }

        if stream.is_none() {
            match TcpStream::connect(config.addr.as_ref().unwrap()).await {
                Ok(s) => stream = Some(s),
                Err(_) => { config.addr = None; continue; }
            }
        }

        save_config(&config);
        let name = config.name.clone().unwrap();
        let addr = config.addr.clone().unwrap();

        let (reader, mut writer) = stream.take().unwrap().into_split();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel::<ServerMsg>();
        let (client_tx, mut client_rx) = mpsc::unbounded_channel::<ClientMsg>();

        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 { break; }
                if let Ok(msg) = serde_json::from_str::<ServerMsg>(&line) { let _ = server_tx.send(msg); }
                line.clear();
            }
        });

        tokio::spawn(async move {
            while let Some(msg) = client_rx.recv().await {
                let j = serde_json::to_string(&msg).unwrap() + "\n";
                let _ = writer.write_all(j.as_bytes()).await;
            }
        });

        client_tx.send(ClientMsg::SetName(name.clone())).ok();

        'lobby: loop {
            loop {
                client_tx.send(ClientMsg::GetRooms).ok();
                client_tx.send(ClientMsg::GetUsers).ok();
                let mut rooms = vec![];
                let mut users = vec![];
                let start = std::time::Instant::now();
                while start.elapsed() < Duration::from_millis(300) {
                    if let Ok(msg) = server_rx.try_recv() {
                        match msg {
                            ServerMsg::RoomList(list) => rooms = list,
                            ServerMsg::UserList(list) => users = list,
                            _ => {}
                        }
                    }
                    tokio::task::yield_now().await;
                }

                println!("\n[ Lobby ] Online: {}", users.join(", "));
                let options = vec!["New Room", "Join Room", "Refresh", "Back to Name Entry"];
                match Select::new("Action:", options).prompt() {
                    Ok("Back to Name Entry") => { config.name = None; continue 'outer; }
                    Ok("New Room") => { client_tx.send(ClientMsg::CreateRoom).ok(); break; }
                    Ok("Join Room") => {
                        let mut room_options = rooms.clone();
                        room_options.push("<-- Back".to_string());
                        match Select::new("Select Room:", room_options).prompt() {
                            Ok(id) if id == "<-- Back" => continue,
                            Ok(id) => { client_tx.send(ClientMsg::JoinRoom(id)).ok(); break; }
                            Err(InquireError::OperationCanceled) => { show_stealth_mode(Some(&addr)).await?; }
                            Err(_) => std::process::exit(0),
                        }
                    }
                    Ok(_) => continue,
                    Err(InquireError::OperationCanceled) => { show_stealth_mode(Some(&addr)).await?; }
                    Err(_) => std::process::exit(0),
                }
            }

            let mut game = GameState {
                cursor: (7, 7),
                board: [[Piece::None; BOARD_SIZE]; BOARD_SIZE],
                my_piece: Piece::None,
                turn: Piece::Black,
                room_id: String::new(),
                opp_ready: false,
                ready: false,
                winner: None,
                winner_name: String::new(),
                opponent_name: "Waiting...".into(),
                my_name: name.clone(),
            };

            enable_raw_mode()?;
            let mut out = BufWriter::new(io::stdout());
            execute!(out, EnterAlternateScreen, cursor::Hide, Clear(ClearType::All))?;

            let mut exit_to_lobby = false;
            let mut events = EventStream::new();

            loop {
                draw_game_fast(&mut out, &game)?;

                tokio::select! {
                    Some(Ok(ev)) = events.next() => {
                        if let Event::Key(key) = ev {
                            if key.kind != KeyEventKind::Press { continue; }
                            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) { std::process::exit(0); }
                            if key.code == KeyCode::Esc {
                                show_stealth_mode(Some(&addr)).await?;
                                enable_raw_mode()?;
                                execute!(out, EnterAlternateScreen, cursor::Hide, Clear(ClearType::All))?;
                                events = EventStream::new();
                                continue;
                            }

                            if game.winner.is_some() {
                                disable_raw_mode()?; execute!(out, LeaveAlternateScreen, cursor::Show)?;
                                let menu_title = format!("WINNER: {} | Action:", game.winner_name);
                                match Select::new(&menu_title, vec!["Rematch", "Leave Room"]).prompt() {
                                    Ok("Rematch") => {
                                        game.winner = None; game.ready = true; game.opp_ready = false;
                                        game.board = [[Piece::None; BOARD_SIZE]; BOARD_SIZE];
                                        client_tx.send(ClientMsg::Ready).ok();
                                        enable_raw_mode()?; execute!(out, EnterAlternateScreen, cursor::Hide, Clear(ClearType::All))?;
                                        events = EventStream::new();
                                    }
                                    _ => { client_tx.send(ClientMsg::LeaveRoom).ok(); exit_to_lobby = true; break; }
                                }
                            } else {
                                match key.code {
                                    KeyCode::Up if game.cursor.0 > 0 => game.cursor.0 -= 1,
                                    KeyCode::Down if game.cursor.0 < BOARD_SIZE - 1 => game.cursor.0 += 1,
                                    KeyCode::Left if game.cursor.1 > 0 => game.cursor.1 -= 1,
                                    KeyCode::Right if game.cursor.1 < BOARD_SIZE - 1 => game.cursor.1 += 1,
                                    KeyCode::Enter => {
                                        if game.my_piece != Piece::None && game.board[game.cursor.0][game.cursor.1] == Piece::None && game.turn == game.my_piece {
                                            client_tx.send(ClientMsg::Move(game.cursor.0, game.cursor.1)).ok();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Some(msg) = server_rx.recv() => {
                        match msg {
                            ServerMsg::RoomCreated(id) | ServerMsg::RoomJoined(id) => game.room_id = id,
                            ServerMsg::GameStart { player_id, opponent_name } => {
                                game.my_piece = player_id;
                                game.opponent_name = opponent_name;
                                game.board = [[Piece::None; BOARD_SIZE]; BOARD_SIZE];
                                game.turn = Piece::Black;
                                game.winner = None;
                            }
                            ServerMsg::UpdateBoard { r, c, p } => {
                                game.board[r][c] = p;
                                game.turn = if p == Piece::Black { Piece::White } else { Piece::Black };
                            }
                            ServerMsg::GameOver { winner } => {
                                game.winner = Some("DONE".into());
                                game.winner_name = if winner == game.my_piece { "YOU".into() } else { game.opponent_name.clone() };
                            }
                            ServerMsg::OpponentReady => game.opp_ready = true,
                            ServerMsg::Error(e) => {
                                disable_raw_mode()?; execute!(out, LeaveAlternateScreen, cursor::Show)?;
                                println!("ERR: {}", e);
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                exit_to_lobby = true; break;
                            }
                            _ => {}
                        }
                        draw_game_fast(&mut out, &game)?;
                    }
                }
                if exit_to_lobby { break; }
            }
            disable_raw_mode()?;
            execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
            if exit_to_lobby { continue 'lobby; }
        }
    }
}

async fn show_stealth_mode(addr: Option<&str>) -> io::Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, cursor::Hide, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let server = addr.unwrap_or("127.0.0.1:8080");
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut logs = String::with_capacity(8192);
    use std::fmt::Write as _;
    let _ = writeln!(logs, "[{}] [SYS] Initializing kernel subsystems...", now);
    let _ = writeln!(logs, "[{}] [INFO] Established connection to gateway: {}", now, server);
    let _ = writeln!(logs, "[{}] [DEBUG] Handshake protocol v2.4 verified. (0x{:X})", now, rand::random::<u32>());
    let _ = writeln!(logs, "[{}] [INFO] Node status: ONLINE | NodeID: 0x{:X}", now, rand::random::<u16>());
    let _ = writeln!(logs, "[{}] [TRACE] Raw packet capture sequence synchronized.", now);
    let _ = writeln!(logs, "\n(Active session monitoring... Press ESC to resume debugging context.)");
    stdout.write_all(logs.as_bytes())?;
    stdout.flush()?;
    let mut events = EventStream::new();
    loop {
        tokio::select! {
            Some(Ok(Event::Key(key))) = events.next() => {
                if key.kind != KeyEventKind::Press { continue; }
                if key.code == KeyCode::Esc { break; }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) { std::process::exit(0); }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    execute!(stdout, LeaveAlternateScreen, cursor::Show)?;
    disable_raw_mode()?;
    Ok(())
}

fn draw_game_fast<W: io::Write>(out: &mut W, state: &GameState) -> io::Result<()> {
    use std::fmt::Write as _;
    queue!(out, cursor::MoveTo(0, 0))?;
    let mut buf = String::with_capacity(16384);
    let now = Local::now().format("%H:%M:%S");

    let _ = writeln!(buf, "[{}] [SYS] Kernel_Init: 0xBC00 | Node: ONLINE | RID: {}", now, state.room_id);
    let _ = writeln!(buf, "[{}] [INFO] Thread_Pool: READY | Master_Signal: STABLE", now);

    for r in 0..BOARD_SIZE {
        let _ = write!(buf, "[{}] [DEBUG] HEX_DATA_SEGMENT_0x{:X} | op_{:02} | ", now, 0xE000 + r * 16, r);
        for c in 0..BOARD_SIZE {
            let symbol = match state.board[r][c] {
                Piece::None => ".",
                Piece::Black => "1",
                Piece::White => "0",
            };
            if (r, c) == state.cursor {
                buf.push_str("\x1b[7m"); buf.push_str(symbol); buf.push_str("\x1b[0m");
            } else {
                buf.push_str(symbol);
            }
            buf.push_str("  ");
        }
        let _ = writeln!(buf, "| ck_{:02X}", r * 7 + 0xA1);
    }

    buf.push_str("\r\n");
    if let Some(_) = &state.winner {
        let _ = writeln!(buf, "[{}] [WARN] SESSION_DONE: WINNER_NAME: {} (Input Required)   ", now, state.winner_name);
    } else if state.my_piece == Piece::None {
        let _ = writeln!(buf, "[{}] [IDLE] NetStack: Awaiting peer connection...   ", now);
    } else {
        let turn_log = if state.turn == state.my_piece { "LOCAL_IO_FOCUS" } else { "REMOTE_POLL_WAIT" };
        let my_p = if state.my_piece == Piece::Black { "1" } else { "0" };
        let _ = writeln!(buf, "[{}] [TRACE] EventDispatcher: State={} | Piece: {}   ", now, turn_log, my_p);
    }
    let _ = writeln!(buf, "[{}] [INFO] Identity_Auth: LOCAL_NODE<{}> <-> PEER_NODE<{}>   ", now, state.my_name, state.opponent_name);
    let _ = writeln!(buf, "[{}] [INFO] Background worker active. Press ESC for diag logs.", now);
    
    out.write_all(buf.as_bytes())?;
    out.flush()?;
    Ok(())
}
