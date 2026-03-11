#[path = "../common.rs"]
mod common;
use common::*;

use std::{collections::HashMap, sync::{Arc, Mutex}};
use tokio::{net::{TcpListener, TcpStream}, io::{AsyncBufReadExt, AsyncWriteExt, BufReader}};
use serde_json;

struct Player {
    tx: tokio::sync::mpsc::UnboundedSender<ServerMsg>,
    name: String,
    room_id: Option<String>,
}

struct Room {
    players: Vec<usize>,
    ready: Vec<bool>,
    board: [[Piece; BOARD_SIZE]; BOARD_SIZE],
    turn: Piece,
}

type Clients = Arc<Mutex<HashMap<usize, Player>>>;
type Rooms = Arc<Mutex<HashMap<String, Room>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut port = 10031;
    let listener = loop {
        let addr = format!("127.0.0.1:{}", port);
        match TcpListener::bind(&addr).await {
            Ok(l) => {
                let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                println!("[{}] [INFO] Server started on {}", now, addr);
                break l;
            }
            Err(_) if port < 8100 => { port += 1; continue; }
            Err(e) => return Err(e.into()),
        }
    };

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));
    let mut next_id = 0;

    loop {
        let (socket, addr) = listener.accept().await?;
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        println!("[{}] [INFO] New connection from {}", now, addr);
        
        let clients_c = clients.clone();
        let rooms_c = rooms.clone();
        let id = next_id;
        next_id += 1;

        tokio::spawn(async move {
            let _ = handle_client(socket, id, clients_c.clone(), rooms_c.clone()).await;
            // 掉线时的彻底清理
            let mut cls = clients_c.lock().unwrap();
            if let Some(p) = cls.remove(&id) {
                if let Some(rid) = p.room_id {
                    remove_player_from_room(id, &rid, &mut cls, &rooms_c);
                }
            }
        });
    }
}

fn remove_player_from_room(id: usize, rid: &str, cls: &mut HashMap<usize, Player>, rooms: &Rooms) {
    let mut rs = rooms.lock().unwrap();
    if let Some(room) = rs.get_mut(rid) {
        room.players.retain(|&pid| pid != id);
        // 通知幸存者
        for &opp_id in &room.players {
            if let Some(opp) = cls.get(&opp_id) {
                let _ = opp.tx.send(ServerMsg::Error("Opponent disconnected/left.".into()));
            }
        }
        if room.players.is_empty() {
            rs.remove(rid);
            let now = chrono::Local::now().format("%H:%M:%S");
            println!("[{}] [INFO] Room {} dissolved (No users remaining).", now, rid);
        }
    }
}

async fn handle_client(socket: TcpStream, id: usize, clients: Clients, rooms: Rooms) -> anyhow::Result<()> {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    
    clients.lock().unwrap().insert(id, Player { tx, name: format!("User_{}", id), room_id: None });

    let mut line = String::new();
    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                if result? == 0 { break; }
                if let Ok(msg) = serde_json::from_str::<ClientMsg>(&line) {
                    handle_msg(id, msg, &clients, &rooms).await?;
                }
                line.clear();
            }
            Some(msg) = rx.recv() => {
                let s = serde_json::to_string(&msg)? + "\n";
                let _ = writer.write_all(s.as_bytes()).await;
            }
        }
    }
    Ok(())
}

async fn handle_msg(id: usize, msg: ClientMsg, clients: &Clients, rooms: &Rooms) -> anyhow::Result<()> {
    match msg {
        ClientMsg::SetName(name) => {
            let mut cls = clients.lock().unwrap();
            if let Some(p) = cls.get_mut(&id) {
                p.name = name;
                let _ = p.tx.send(ServerMsg::Welcome("Connected.".into()));
            }
        }
        ClientMsg::CreateRoom => {
            let rid = format!("{:04}", rand::random::<u16>());
            let mut rs = rooms.lock().unwrap();
            rs.insert(rid.clone(), Room {
                players: vec![id],
                ready: vec![false, false],
                board: [[Piece::None; BOARD_SIZE]; BOARD_SIZE],
                turn: Piece::Black,
            });
            let mut cls = clients.lock().unwrap();
            if let Some(p) = cls.get_mut(&id) {
                p.room_id = Some(rid.clone());
                let _ = p.tx.send(ServerMsg::RoomCreated(rid));
            }
        }
        ClientMsg::JoinRoom(rid) => {
            let mut rs = rooms.lock().unwrap();
            if let Some(room) = rs.get_mut(&rid) {
                if room.players.len() < 2 {
                    room.players.push(id);
                    let mut cls = clients.lock().unwrap();
                    if let Some(p) = cls.get_mut(&id) {
                        p.room_id = Some(rid.clone());
                        let _ = p.tx.send(ServerMsg::RoomJoined(rid.clone()));
                    }
                    if room.players.len() == 2 { start_game(room, &cls); }
                } else {
                    let cls = clients.lock().unwrap();
                    let _ = cls.get(&id).unwrap().tx.send(ServerMsg::Error("Full".into()));
                }
            } else {
                let cls = clients.lock().unwrap();
                let _ = cls.get(&id).unwrap().tx.send(ServerMsg::Error("Not found".into()));
            }
        }
        ClientMsg::Ready => {
            let mut rs = rooms.lock().unwrap();
            let cls = clients.lock().unwrap();
            if let Some(p) = cls.get(&id) {
                if let Some(rid) = &p.room_id {
                    if let Some(room) = rs.get_mut(rid) {
                        if let Some(pos) = room.players.iter().position(|&pid| pid == id) {
                            room.ready[pos] = true;
                            if room.ready.iter().take(room.players.len()).all(|&r| r) && room.players.len() == 2 {
                                start_game(room, &cls);
                            }
                        }
                    }
                }
            }
        }
        ClientMsg::LeaveRoom => {
            let mut cls = clients.lock().unwrap();
            if let Some(p) = cls.get_mut(&id) {
                if let Some(rid) = p.room_id.take() {
                    remove_player_from_room(id, &rid, &mut cls, rooms);
                }
            }
        }
        ClientMsg::Move(r, c) => {
            let mut rs = rooms.lock().unwrap();
            let cls = clients.lock().unwrap();
            if let Some(p) = cls.get(&id) {
                if let Some(rid) = &p.room_id {
                    if let Some(room) = rs.get_mut(rid) {
                        let pos = room.players.iter().position(|&pid| pid == id).unwrap();
                        let my_piece = if pos == 0 { Piece::Black } else { Piece::White };
                        if room.turn == my_piece && room.board[r][c] == Piece::None {
                            room.board[r][c] = my_piece;
                            room.turn = if my_piece == Piece::Black { Piece::White } else { Piece::Black };
                            let win = check_win(&room.board, r, c, my_piece);
                            for &pid in &room.players {
                                let _ = cls.get(&pid).unwrap().tx.send(ServerMsg::UpdateBoard { r, c, p: my_piece });
                                if win { let _ = cls.get(&pid).unwrap().tx.send(ServerMsg::GameOver { winner: my_piece }); }
                            }
                        }
                    }
                }
            }
        }
        ClientMsg::GetRooms => {
            let rs = rooms.lock().unwrap();
            let list: Vec<String> = rs.keys().cloned().collect();
            let cls = clients.lock().unwrap();
            let _ = cls.get(&id).unwrap().tx.send(ServerMsg::RoomList(list));
        }
        ClientMsg::GetUsers => {
            let cls = clients.lock().unwrap();
            let names: Vec<String> = cls.values().map(|p| p.name.clone()).collect();
            let _ = cls.get(&id).unwrap().tx.send(ServerMsg::UserList(names));
        }
    }
    Ok(())
}

fn start_game(room: &mut Room, cls: &HashMap<usize, Player>) {
    room.board = [[Piece::None; BOARD_SIZE]; BOARD_SIZE];
    room.turn = Piece::Black;
    room.ready = vec![false, false];
    let p1_name = cls.get(&room.players[0]).map(|p| p.name.clone()).unwrap_or_default();
    let p2_name = cls.get(&room.players[1]).map(|p| p.name.clone()).unwrap_or_default();
    let _ = cls.get(&room.players[0]).unwrap().tx.send(ServerMsg::GameStart { player_id: Piece::Black, opponent_name: p2_name });
    let _ = cls.get(&room.players[1]).unwrap().tx.send(ServerMsg::GameStart { player_id: Piece::White, opponent_name: p1_name });
}
