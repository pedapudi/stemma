//! Exact dense search used when the optional sidecar is not compiled.

use std::path::PathBuf;

use stemmadb::StemmaDb;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] stemmadb::rusqlite::Error),
    #[error("invalid dense search state: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Neighbor {
    pub rowid: i64,
    pub cosine: f64,
    pub approximate: bool,
}

pub struct DenseSearch;

impl Default for DenseSearch {
    fn default() -> Self {
        Self::exact()
    }
}

impl DenseSearch {
    pub fn exact() -> Self {
        Self
    }

    pub fn usearch(root: impl Into<PathBuf>) -> Result<Self> {
        let _ = root;
        Err(Error::Invalid(
            "sidecar mode requires the usearch-sidecar build feature".into(),
        ))
    }

    pub fn rebuild(&self, _db: &StemmaDb) -> Result<()> {
        Ok(())
    }

    pub(crate) fn search(
        &self,
        db: &StemmaDb,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<Neighbor>> {
        exact(db, query, limit)
    }

    pub(crate) fn search_exact(
        &self,
        db: &StemmaDb,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<Neighbor>> {
        exact(db, query, limit)
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
        .collect::<std::result::Result<_, _>>()?;
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_configuration_requires_the_build_feature() {
        assert!(DenseSearch::usearch("vectors").is_err());
    }
}
