use serde::{Deserialize, Serialize};

pub const BOARD_SIZE: usize = 15;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub enum Piece {
    None,
    Black, // 1
    White, // 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    SetName(String),
    CreateRoom,
    JoinRoom(String),
    Ready,
    Move(usize, usize),
    GetRooms,
    GetUsers,
    LeaveRoom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome(String),
    RoomCreated(String),
    RoomJoined(String),
    RoomList(Vec<String>),
    UserList(Vec<String>),
    GameStart { player_id: Piece, opponent_name: String },
    UpdateBoard { r: usize, c: usize, p: Piece }, // 修改点：明确带上棋子类型
    GameOver { winner: Piece },
    Error(String),
    OpponentReady,
}

pub fn check_win(board: &[[Piece; BOARD_SIZE]; BOARD_SIZE], r: usize, c: usize, p: Piece) -> bool {
    let dirs = [(0, 1), (1, 0), (1, 1), (1, -1)];
    for (dr, dc) in dirs {
        let mut count = 1;
        for i in 1..5 {
            let nr = r as isize + dr * i;
            let nc = c as isize + dc * i;
            if nr >= 0 && nr < BOARD_SIZE as isize && nc >= 0 && nc < BOARD_SIZE as isize {
                if board[nr as usize][nc as usize] == p { count += 1; } else { break; }
            } else { break; }
        }
        for i in 1..5 {
            let nr = r as isize - dr * i;
            let nc = c as isize - dc * i;
            if nr >= 0 && nr < BOARD_SIZE as isize && nc >= 0 && nc < BOARD_SIZE as isize {
                if board[nr as usize][nc as usize] == p { count += 1; } else { break; }
            } else { break; }
        }
        if count >= 5 { return true; }
    }
    false
}
