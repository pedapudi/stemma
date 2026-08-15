//! Dense candidate generation. SQLite is the exact oracle; the optional
//! sidecar only proposes vector rowids, which are rescored from SQLite.

use std::io::Read;
use std::path::PathBuf;
use std::{path::Path, sync::Mutex};

use stemmadb::StemmaDb;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const TABLE: &str = "vec_dense";
const METRIC: &str = "l2sq";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] stemmadb::rusqlite::Error),
    #[error("ingest error: {0}")]
    Ingest(#[from] stemma_ingest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sidecar error: {0}")]
    Sidecar(String),
    #[error("invalid dense search state: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One SQLite vector row, ordered by authoritative cosine then rowid.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Neighbor {
    pub rowid: i64,
    pub cosine: f64,
    pub approximate: bool,
}

/// The generation identity stored in SQLite for one rebuildable sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Receipt {
    pub corpus_fingerprint: String,
    pub vector_revision: String,
    pub vector_generation: u64,
    pub vector_count: usize,
    pub dimension: usize,
    pub metric: String,
    pub checksum: String,
}

/// Exact search by default, or one opt-in document-vector sidecar.
pub struct DenseSearch {
    root: Option<PathBuf>,
    loaded: Mutex<Option<Loaded>>,
}

struct Loaded {
    receipt: Receipt,
    index: Index,
}

impl Default for DenseSearch {
    fn default() -> Self {
        Self::exact()
    }
}

impl DenseSearch {
    pub fn exact() -> Self {
        Self {
            root: None,
            loaded: Mutex::new(None),
        }
    }

    pub fn usearch(root: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            root: Some(root.into()),
            loaded: Mutex::new(None),
        })
    }

    /// Builds the sidecar in SQLite rowid order and records its generation.
    pub fn rebuild(&self, db: &StemmaDb) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let generation = generation(db)?;
        if generation.vector_count == 0 {
            db.conn().execute(
                "DELETE FROM vector_sidecar_receipts WHERE vector_table = ?1",
                [TABLE],
            )?;
            *self.loaded.lock().unwrap() = None;
            return Ok(());
        }
        if let Some(receipt) = read_receipt(db)? {
            if same_generation(&generation, &receipt) {
                let cached = self
                    .loaded
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|entry| entry.receipt == receipt);
                if cached || self.load(root, &receipt).is_ok() {
                    return Ok(());
                }
            }
        }

        std::fs::create_dir_all(root)?;
        let temporary = root.join(format!("{TABLE}.{}.tmp", std::process::id()));
        let options = IndexOptions {
            dimensions: generation.dimension,
            metric: MetricKind::L2sq,
            quantization: ScalarKind::F32,
            connectivity: 0,
            expansion_add: 0,
            expansion_search: 0,
            multi: false,
        };
        let index = Index::new(&options).map_err(sidecar_error)?;
        index
            .reserve(generation.vector_count)
            .map_err(sidecar_error)?;
        let mut statement = db
            .conn()
            .prepare("SELECT rowid, embedding FROM vec_dense ORDER BY rowid")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (rowid, bytes) = row?;
            let key = u64::try_from(rowid)
                .map_err(|_| Error::Invalid(format!("negative vector rowid {rowid}")))?;
            let vector = decode_vector(&bytes, generation.dimension)?;
            index.add(key, &vector).map_err(sidecar_error)?;
        }
        index
            .save(path_string(&temporary)?)
            .map_err(sidecar_error)?;
        let built_checksum = checksum(&temporary)?;
        let path = sidecar_path(root, &built_checksum);
        if path.exists() {
            if checksum(&path)? == built_checksum {
                std::fs::remove_file(&temporary)?;
            } else {
                let damaged = root.join(format!("{TABLE}.{}.damaged", std::process::id()));
                std::fs::rename(&path, &damaged)?;
                if let Err(error) = std::fs::rename(&temporary, &path) {
                    let _ = std::fs::rename(&damaged, &path);
                    return Err(error.into());
                }
                std::fs::remove_file(damaged)?;
            }
        } else {
            std::fs::rename(&temporary, &path)?;
        }
        let receipt = Receipt {
            checksum: built_checksum,
            ..generation
        };
        write_receipt(db, &receipt)?;
        *self.loaded.lock().unwrap() = Some(Loaded {
            receipt: receipt.clone(),
            index,
        });
        Ok(())
    }

    /// Searches the sidecar when its file and SQLite receipt match the live
    /// vector generation. Every mismatch or sidecar error uses exact SQLite.
    pub(crate) fn search(
        &self,
        db: &StemmaDb,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<Neighbor>> {
        if self.root.is_none() {
            return exact(db, query, limit);
        }
        match self.approximate(db, query, limit) {
            Ok(hits) => Ok(hits),
            Err(error) => {
                tracing::warn!(error = %error, "dense sidecar rejected; using exact search");
                exact(db, query, limit)
            }
        }
    }

    pub(crate) fn search_exact(
        &self,
        db: &StemmaDb,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<Neighbor>> {
        exact(db, query, limit)
    }

    fn approximate(&self, db: &StemmaDb, query: &[f32], limit: usize) -> Result<Vec<Neighbor>> {
        let root = self.root.as_ref().expect("checked above");
        let active = generation(db)?;
        let receipt = read_receipt(db)?.ok_or_else(|| Error::Invalid("missing receipt".into()))?;
        if !same_generation(&active, &receipt) {
            return Err(Error::Invalid("stale receipt".into()));
        }
        if query.len() != receipt.dimension {
            return Err(Error::Invalid(format!(
                "query dimension {} does not match {}",
                query.len(),
                receipt.dimension
            )));
        }

        let mut loaded = self.loaded.lock().unwrap();
        if !loaded
            .as_ref()
            .is_some_and(|entry| entry.receipt == receipt)
        {
            drop(loaded);
            self.load(root, &receipt)?;
            loaded = self.loaded.lock().unwrap();
        }
        let count = limit.saturating_mul(4).min(receipt.vector_count);
        let keys = loaded
            .as_ref()
            .expect("sidecar loaded above")
            .index
            .search(query, count)
            .map_err(sidecar_error)?
            .keys;
        rescore(db, query, &keys, limit)
    }

    fn load(&self, root: &Path, receipt: &Receipt) -> Result<()> {
        let path = sidecar_path(root, &receipt.checksum);
        if checksum(&path)? != receipt.checksum {
            return Err(Error::Invalid("sidecar checksum mismatch".into()));
        }
        let index = Index::restore(path_string(&path)?).map_err(sidecar_error)?;
        if index.dimensions() != receipt.dimension
            || index.size() != receipt.vector_count
            || index.metric_kind() != MetricKind::L2sq
            || index.scalar_kind() != ScalarKind::F32
        {
            return Err(Error::Invalid("sidecar header mismatch".into()));
        }
        *self.loaded.lock().unwrap() = Some(Loaded {
            receipt: receipt.clone(),
            index,
        });
        Ok(())
    }
}

fn exact(db: &StemmaDb, query: &[f32], limit: usize) -> Result<Vec<Neighbor>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let blob: Vec<u8> = query.iter().flat_map(|value| value.to_le_bytes()).collect();
    let mut statement = db.conn().prepare_cached(
        "SELECT rowid, distance FROM vec_dense WHERE embedding MATCH ?1 AND k = ?2",
    )?;
    let hits = statement
        .query_map(stemmadb::rusqlite::params![blob, limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?
        .map(|row| {
            row.map(|(rowid, distance)| Neighbor {
                rowid,
                cosine: 1.0 - distance * distance / 2.0,
                approximate: false,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(hits)
}

fn rescore(db: &StemmaDb, query: &[f32], keys: &[u64], limit: usize) -> Result<Vec<Neighbor>> {
    let mut statement = db
        .conn()
        .prepare_cached("SELECT embedding FROM vec_dense WHERE rowid = ?1")?;
    let mut hits = Vec::with_capacity(keys.len());
    for &key in keys {
        let rowid = i64::try_from(key)
            .map_err(|_| Error::Invalid(format!("vector key {key} is out of range")))?;
        let bytes: Vec<u8> = match statement.query_row([rowid], |row| row.get(0)) {
            Ok(bytes) => bytes,
            Err(stemmadb::rusqlite::Error::QueryReturnedNoRows) => continue,
            Err(error) => return Err(error.into()),
        };
        let vector = decode_vector(&bytes, query.len())?;
        let distance = query
            .iter()
            .zip(vector)
            .map(|(left, right)| f64::from(*left - right).powi(2))
            .sum::<f64>()
            .sqrt();
        hits.push(Neighbor {
            rowid,
            cosine: 1.0 - distance * distance / 2.0,
            approximate: true,
        });
    }
    hits.sort_by(|left, right| {
        right
            .cosine
            .total_cmp(&left.cosine)
            .then(left.rowid.cmp(&right.rowid))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn generation(db: &StemmaDb) -> Result<Receipt> {
    let corpus_fingerprint = stemma_ingest::corpus_fingerprint(db.conn())?;
    db.conn().execute(
        "INSERT OR IGNORE INTO vector_generations VALUES (?1, 1)",
        [TABLE],
    )?;
    let (backend, model, revision, dimension, quantization, query_template, card_format) =
        db.conn().query_row(
            "SELECT backend, model, revision, dimension, quantization,
                    query_template, card_format
             FROM model_registry WHERE vector_table = ?1",
            [TABLE],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
    let vector_generation: i64 = db.conn().query_row(
        "SELECT generation FROM vector_generations WHERE vector_table = ?1",
        [TABLE],
        |row| row.get(0),
    )?;
    let vector_revision = stemma_ingest::content_hash(
        &serde_json::to_string(&(
            backend,
            model,
            revision,
            dimension,
            quantization,
            query_template,
            card_format,
            vector_generation,
        ))
        .map_err(|error| Error::Invalid(error.to_string()))?,
    );
    let vector_count: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM vec_dense", [], |row| row.get(0))?;
    Ok(Receipt {
        corpus_fingerprint,
        vector_revision,
        vector_generation: u64::try_from(vector_generation)
            .map_err(|_| Error::Invalid("negative vector generation".into()))?,
        vector_count: usize::try_from(vector_count)
            .map_err(|_| Error::Invalid("negative vector count".into()))?,
        dimension: usize::try_from(dimension)
            .map_err(|_| Error::Invalid("negative vector dimension".into()))?,
        metric: METRIC.into(),
        checksum: String::new(),
    })
}

fn read_receipt(db: &StemmaDb) -> Result<Option<Receipt>> {
    let row = db.conn().query_row(
        "SELECT corpus_fingerprint, vector_revision, vector_generation,
                vector_count, dimension, metric, checksum
         FROM vector_sidecar_receipts WHERE vector_table = ?1",
        [TABLE],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        },
    );
    match row {
        Ok((
            corpus_fingerprint,
            vector_revision,
            generation,
            count,
            dimension,
            metric,
            checksum,
        )) => Ok(Some(Receipt {
            corpus_fingerprint,
            vector_revision,
            vector_generation: u64::try_from(generation)
                .map_err(|_| Error::Invalid("negative receipt generation".into()))?,
            vector_count: usize::try_from(count)
                .map_err(|_| Error::Invalid("negative receipt count".into()))?,
            dimension: usize::try_from(dimension)
                .map_err(|_| Error::Invalid("negative receipt dimension".into()))?,
            metric,
            checksum,
        })),
        Err(stemmadb::rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_receipt(db: &StemmaDb, receipt: &Receipt) -> Result<()> {
    db.conn().execute(
        "INSERT INTO vector_sidecar_receipts
             (vector_table, corpus_fingerprint, vector_revision, vector_generation,
              vector_count, dimension, metric, checksum)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(vector_table) DO UPDATE SET
             corpus_fingerprint = excluded.corpus_fingerprint,
             vector_revision = excluded.vector_revision,
             vector_generation = excluded.vector_generation,
             vector_count = excluded.vector_count,
             dimension = excluded.dimension,
             metric = excluded.metric,
             checksum = excluded.checksum,
             built_at = datetime('now')",
        stemmadb::rusqlite::params![
            TABLE,
            receipt.corpus_fingerprint,
            receipt.vector_revision,
            receipt.vector_generation as i64,
            receipt.vector_count as i64,
            receipt.dimension as i64,
            receipt.metric,
            receipt.checksum,
        ],
    )?;
    Ok(())
}

fn decode_vector(bytes: &[u8], dimension: usize) -> Result<Vec<f32>> {
    if bytes.len() != dimension.saturating_mul(4) {
        return Err(Error::Invalid(format!(
            "vector has {} bytes, expected {}",
            bytes.len(),
            dimension.saturating_mul(4)
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn sidecar_path(root: &Path, checksum: &str) -> PathBuf {
    root.join(format!("{TABLE}.{checksum}.usearch"))
}

fn path_string(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::Invalid(format!("non-Unicode path {}", path.display())))
}

fn checksum(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut state = 0xcbf29ce484222325u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            state ^= u64::from(*byte);
            state = state.wrapping_mul(0x100000001b3);
        }
    }
    Ok(format!("{state:016x}"))
}

fn same_generation(left: &Receipt, right: &Receipt) -> bool {
    left.corpus_fingerprint == right.corpus_fingerprint
        && left.vector_revision == right.vector_revision
        && left.vector_generation == right.vector_generation
        && left.vector_count == right.vector_count
        && left.dimension == right.dimension
        && left.metric == right.metric
}

fn sidecar_error(error: impl std::fmt::Display) -> Error {
    Error::Sidecar(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(values: [f32; 4]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn database() -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.docs(id INTEGER PRIMARY KEY, body TEXT);
                 INSERT INTO src.docs VALUES (1, 'alpha document'), (2, 'beta document');",
            )
            .unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        db.conn()
            .execute_batch(
                "CREATE VIRTUAL TABLE vec_dense USING vec0(
                     embedding float[4], src_table text, src_column text, src_rowid integer);
                 INSERT INTO model_registry
                     (vector_table, backend, model, revision, dimension, quantization)
                 VALUES ('vec_dense', 'test', 'encoder', 'one', 4, 'f32');",
            )
            .unwrap();
        db.conn()
            .execute("INSERT INTO vector_generations VALUES ('vec_dense', 1)", [])
            .unwrap();
        for (rowid, embedding) in [
            (1, vector([1.0, 0.0, 0.0, 0.0])),
            (2, vector([0.0, 1.0, 0.0, 0.0])),
            (3, vector([0.7, 0.7, 0.0, 0.0])),
        ] {
            db.conn()
                .execute(
                    "INSERT INTO vec_dense
                         (embedding, src_table, src_column, src_rowid)
                     VALUES (?1, 'docs', 'body', ?2)",
                    stemmadb::rusqlite::params![embedding, rowid],
                )
                .unwrap();
        }
        db
    }

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "stemma-dense-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn sidecar_build_reopen_and_exact_rescore_are_deterministic() {
        let db = database();
        let root = root("reopen");
        std::fs::remove_dir_all(&root).ok();
        let search = DenseSearch::usearch(&root).unwrap();
        search.rebuild(&db).unwrap();
        let first = read_receipt(&db).unwrap().unwrap();
        let approximate = search.search(&db, &[1.0, 0.0, 0.0, 0.0], 3).unwrap();
        let exact = search.search_exact(&db, &[1.0, 0.0, 0.0, 0.0], 3).unwrap();
        assert!(approximate.iter().all(|hit| hit.approximate));
        assert_eq!(approximate[0].rowid, exact[0].rowid);
        assert_eq!(approximate[0].cosine, exact[0].cosine);

        let reopened = DenseSearch::usearch(&root)
            .unwrap()
            .search(&db, &[1.0, 0.0, 0.0, 0.0], 3)
            .unwrap();
        assert_eq!(approximate, reopened);
        search.rebuild(&db).unwrap();
        let second = read_receipt(&db).unwrap().unwrap();
        assert_eq!(first, second);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn corruption_and_revision_mismatch_fall_back_to_exact() {
        let db = database();
        let root = root("fallback");
        std::fs::remove_dir_all(&root).ok();
        DenseSearch::usearch(&root).unwrap().rebuild(&db).unwrap();
        let receipt = read_receipt(&db).unwrap().unwrap();
        std::fs::write(sidecar_path(&root, &receipt.checksum), b"corrupt").unwrap();
        let corrupt = DenseSearch::usearch(&root)
            .unwrap()
            .search(&db, &[1.0, 0.0, 0.0, 0.0], 2)
            .unwrap();
        assert!(corrupt.iter().all(|hit| !hit.approximate));

        DenseSearch::usearch(&root).unwrap().rebuild(&db).unwrap();
        let repaired = DenseSearch::usearch(&root)
            .unwrap()
            .search(&db, &[1.0, 0.0, 0.0, 0.0], 2)
            .unwrap();
        assert!(repaired.iter().all(|hit| hit.approximate));
        db.conn()
            .execute(
                "UPDATE model_registry SET revision = 'two' WHERE vector_table = 'vec_dense'",
                [],
            )
            .unwrap();
        let stale = DenseSearch::usearch(&root)
            .unwrap()
            .search(&db, &[1.0, 0.0, 0.0, 0.0], 2)
            .unwrap();
        assert!(stale.iter().all(|hit| !hit.approximate));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn same_count_vector_change_invalidates_the_sidecar() {
        let db = database();
        let root = root("generation");
        std::fs::remove_dir_all(&root).ok();
        DenseSearch::usearch(&root).unwrap().rebuild(&db).unwrap();
        db.conn()
            .execute_batch(
                "DELETE FROM vec_dense WHERE rowid = 3;
                 UPDATE vector_generations SET generation = generation + 1
                 WHERE vector_table = 'vec_dense';",
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO vec_dense
                     (embedding, src_table, src_column, src_rowid)
                 VALUES (?1, 'docs', 'body', 3)",
                [vector([0.5, 0.5, 0.5, 0.5])],
            )
            .unwrap();
        let hits = DenseSearch::usearch(&root)
            .unwrap()
            .search(&db, &[1.0, 0.0, 0.0, 0.0], 2)
            .unwrap();
        assert!(hits.iter().all(|hit| !hit.approximate));
        std::fs::remove_dir_all(root).ok();
    }
}
