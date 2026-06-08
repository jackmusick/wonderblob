use crate::error::{Result, StorageError};
use crate::transfer::model::{Direction, Transfer, TransferId, TransferStatus};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use std::sync::Mutex;

/// Fields supplied at enqueue time; the store assigns id/status/timestamps.
pub struct NewTransfer {
    pub connection_id: u64,
    pub direction: Direction,
    pub remote_path: String,
    pub local_path: String,
    pub name: String,
    pub total_bytes: Option<u64>,
}

/// SQLite-backed persistent queue. `Connection` isn't `Sync`, so it lives behind
/// a `Mutex`; locks are held only for the (fast) row read/write, never across
/// network I/O.
pub struct TransferStore {
    conn: Mutex<Connection>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn map_db<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Other {
        detail: format!("transfer store: {e}"),
    }
}

impl TransferStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_db)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_db)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS transfers (
                 id                INTEGER PRIMARY KEY AUTOINCREMENT,
                 connection_id     INTEGER NOT NULL,
                 direction         TEXT    NOT NULL,
                 remote_path       TEXT    NOT NULL,
                 local_path        TEXT    NOT NULL,
                 name              TEXT    NOT NULL,
                 total_bytes       INTEGER,
                 transferred_bytes INTEGER NOT NULL DEFAULT 0,
                 status            TEXT    NOT NULL,
                 error             TEXT,
                 created_at_ms     INTEGER NOT NULL,
                 updated_at_ms     INTEGER NOT NULL
             );",
        )
        .map_err(map_db)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert(&self, t: NewTransfer) -> Result<TransferId> {
        let conn = self.conn.lock().unwrap();
        let now = now_ms();
        conn.execute(
            "INSERT INTO transfers
               (connection_id, direction, remote_path, local_path, name,
                total_bytes, transferred_bytes, status, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 'queued', NULL, ?7, ?7)",
            params![
                t.connection_id as i64,
                t.direction.as_str(),
                t.remote_path,
                t.local_path,
                t.name,
                t.total_bytes.map(|b| b as i64),
                now,
            ],
        )
        .map_err(map_db)?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get(&self, id: TransferId) -> Result<Option<Transfer>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT * FROM transfers WHERE id = ?1",
            params![id],
            row_to_transfer,
        )
        .optional()
        .map_err(map_db)
    }

    /// Newest-first; powers `list_transfers`.
    pub fn list(&self) -> Result<Vec<Transfer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT * FROM transfers ORDER BY created_at_ms DESC, id DESC")
            .map_err(map_db)?;
        let rows = stmt.query_map([], row_to_transfer).map_err(map_db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_db)
    }

    /// Non-terminal rows, oldest-first (so startup re-enqueues in FIFO order).
    pub fn load_incomplete(&self) -> Result<Vec<Transfer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT * FROM transfers
                 WHERE status IN ('queued','running','paused')
                 ORDER BY created_at_ms ASC, id ASC",
            )
            .map_err(map_db)?;
        let rows = stmt.query_map([], row_to_transfer).map_err(map_db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_db)
    }

    pub fn update_progress(&self, id: TransferId, transferred: u64, total: Option<u64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers
                SET transferred_bytes = ?2,
                    total_bytes = COALESCE(?3, total_bytes),
                    updated_at_ms = ?4
              WHERE id = ?1",
            params![id, transferred as i64, total.map(|b| b as i64), now_ms()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    pub fn set_status(&self, id: TransferId, status: TransferStatus, error: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers SET status = ?2, error = ?3, updated_at_ms = ?4 WHERE id = ?1",
            params![id, status.as_str(), error, now_ms()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    /// Upload resume is not supported (see plan header); rewind the offset so a
    /// re-run re-streams the whole file. Single source of the asymmetry.
    pub fn reset_upload_offset(&self, id: TransferId) -> Result<()> {
        self.update_progress(id, 0, None)
    }

    /// Rebind a transfer to a freshly reconnected connection id (restart recovery).
    pub fn rebind_connection(&self, id: TransferId, connection_id: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transfers SET connection_id = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, connection_id as i64, now_ms()],
        )
        .map_err(map_db)?;
        Ok(())
    }

    pub fn delete(&self, id: TransferId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM transfers WHERE id = ?1", params![id])
            .map_err(map_db)?;
        Ok(())
    }

    /// Remove completed rows; returns the number pruned.
    pub fn clear_completed(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM transfers WHERE status = 'completed'", [])
            .map_err(map_db)
    }
}

fn row_to_transfer(row: &Row<'_>) -> rusqlite::Result<Transfer> {
    let dir_s: String = row.get("direction")?;
    let status_s: String = row.get("status")?;
    Ok(Transfer {
        id: row.get("id")?,
        connection_id: row.get::<_, i64>("connection_id")? as u64,
        direction: Direction::from_str(&dir_s).unwrap_or(Direction::Down),
        remote_path: row.get("remote_path")?,
        local_path: row.get("local_path")?,
        name: row.get("name")?,
        total_bytes: row.get::<_, Option<i64>>("total_bytes")?.map(|b| b as u64),
        transferred_bytes: row.get::<_, i64>("transferred_bytes")? as u64,
        status: TransferStatus::from_str(&status_s).unwrap_or(TransferStatus::Failed),
        error: row.get("error")?,
        created_at_ms: row.get("created_at_ms")?,
        updated_at_ms: row.get("updated_at_ms")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::model::{Direction, TransferStatus};

    fn mem() -> TransferStore {
        TransferStore::open_in_memory().expect("open :memory:")
    }

    fn sample(store: &TransferStore, conn: u64, dir: Direction) -> TransferId {
        store
            .insert(NewTransfer {
                connection_id: conn,
                direction: dir,
                remote_path: "/wbtest/a.bin".into(),
                local_path: "/tmp/a.bin".into(),
                name: "a.bin".into(),
                total_bytes: Some(1000),
            })
            .expect("insert")
    }

    #[test]
    fn insert_get_round_trips_as_queued() {
        let s = mem();
        let id = sample(&s, 1, Direction::Down);
        let t = s.get(id).unwrap().unwrap();
        assert_eq!(t.status, TransferStatus::Queued);
        assert_eq!(t.transferred_bytes, 0);
        assert_eq!(t.total_bytes, Some(1000));
        assert_eq!(t.name, "a.bin");
    }

    #[test]
    fn update_progress_and_status_persist() {
        let s = mem();
        let id = sample(&s, 1, Direction::Down);
        s.update_progress(id, 512, Some(1000)).unwrap();
        s.set_status(id, TransferStatus::Running, None).unwrap();
        let t = s.get(id).unwrap().unwrap();
        assert_eq!(t.transferred_bytes, 512);
        assert_eq!(t.status, TransferStatus::Running);
        // updated_at advances on writes.
        assert!(t.updated_at_ms >= t.created_at_ms);
    }

    #[test]
    fn set_status_records_error_text() {
        let s = mem();
        let id = sample(&s, 1, Direction::Up);
        s.set_status(id, TransferStatus::Failed, Some("network reset"))
            .unwrap();
        let t = s.get(id).unwrap().unwrap();
        assert_eq!(t.status, TransferStatus::Failed);
        assert_eq!(t.error.as_deref(), Some("network reset"));
    }

    #[test]
    fn load_incomplete_returns_only_non_terminal() {
        let s = mem();
        let running = sample(&s, 1, Direction::Down);
        let done = sample(&s, 1, Direction::Down);
        let paused = sample(&s, 1, Direction::Up);
        s.set_status(running, TransferStatus::Running, None).unwrap();
        s.set_status(done, TransferStatus::Completed, None).unwrap();
        s.set_status(paused, TransferStatus::Paused, None).unwrap();
        let mut ids: Vec<_> = s
            .load_incomplete()
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec![running, paused]); // completed excluded
    }

    #[test]
    fn list_orders_newest_first_and_clear_completed_prunes() {
        let s = mem();
        let a = sample(&s, 1, Direction::Down);
        let b = sample(&s, 1, Direction::Down);
        s.set_status(a, TransferStatus::Completed, None).unwrap();
        let listed: Vec<_> = s.list().unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(listed, vec![b, a]); // created_at desc (id desc as tiebreak)
        let pruned = s.clear_completed().unwrap();
        assert_eq!(pruned, 1);
        let remaining: Vec<_> = s.list().unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(remaining, vec![b]);
    }
}
