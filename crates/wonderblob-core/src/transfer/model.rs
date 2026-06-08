use serde::Serialize;

pub type TransferId = i64;

/// Transfer direction. `Down` = remote→local, `Up` = local→remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Up,
    Down,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "up" => Some(Direction::Up),
            "down" => Some(Direction::Down),
            _ => None,
        }
    }
}

/// Lifecycle. Terminal states never transition again without an explicit
/// re-enqueue (resume/retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferStatus {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Canceled,
}

impl TransferStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferStatus::Queued => "queued",
            TransferStatus::Running => "running",
            TransferStatus::Paused => "paused",
            TransferStatus::Completed => "completed",
            TransferStatus::Failed => "failed",
            TransferStatus::Canceled => "canceled",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => TransferStatus::Queued,
            "running" => TransferStatus::Running,
            "paused" => TransferStatus::Paused,
            "completed" => TransferStatus::Completed,
            "failed" => TransferStatus::Failed,
            "canceled" => TransferStatus::Canceled,
            _ => return None,
        })
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Canceled
        )
    }
}

/// One row of the `transfers` table; the unit the engine and UI exchange.
/// `transferred_bytes` doubles as the **resume offset** for downloads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: TransferId,
    pub connection_id: u64,
    pub direction: Direction,
    pub remote_path: String,
    pub local_path: String,
    /// Display name (basename of the file being moved).
    pub name: String,
    pub total_bytes: Option<u64>,
    pub transferred_bytes: u64,
    pub status: TransferStatus,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_serializes_camel_case_for_frontend() {
        let t = Transfer {
            id: 7,
            connection_id: 3,
            direction: Direction::Down,
            remote_path: "/wbtest/big.bin".into(),
            local_path: "/home/jack/Downloads/big.bin".into(),
            name: "big.bin".into(),
            total_bytes: Some(1024),
            transferred_bytes: 512,
            status: TransferStatus::Running,
            error: None,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_500,
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["connectionId"], 3);
        assert_eq!(v["direction"], "down");
        assert_eq!(v["status"], "running");
        assert_eq!(v["totalBytes"], 1024);
        assert_eq!(v["transferredBytes"], 512);
    }

    #[test]
    fn status_is_terminal_distinguishes_done_from_active() {
        assert!(TransferStatus::Completed.is_terminal());
        assert!(TransferStatus::Failed.is_terminal());
        assert!(TransferStatus::Canceled.is_terminal());
        assert!(!TransferStatus::Running.is_terminal());
        assert!(!TransferStatus::Queued.is_terminal());
        assert!(!TransferStatus::Paused.is_terminal());
    }

    #[test]
    fn status_round_trips_through_str() {
        for s in [
            TransferStatus::Queued,
            TransferStatus::Running,
            TransferStatus::Paused,
            TransferStatus::Completed,
            TransferStatus::Failed,
            TransferStatus::Canceled,
        ] {
            assert_eq!(TransferStatus::from_str(s.as_str()).unwrap(), s);
        }
    }
}
