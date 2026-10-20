//! Streaming lanes. No compaction: appending pages never moves an existing row.
use crate::domain::{AppError, Commit, ErrorCode, GraphRow, GraphSegment, Result};
const MAX_LANES: usize = 256;
#[derive(Clone)]
struct Track {
    child: String,
    parent: String,
    color: u16,
}
#[derive(Clone, Default)]
pub struct Layout {
    tracks: Vec<Option<Track>>,
    index: u32,
}
impl Layout {
    fn free(&mut self) -> Result<usize> {
        if let Some(i) = self.tracks.iter().position(Option::is_none) {
            return Ok(i);
        }
        if self.tracks.len() == MAX_LANES {
            return Err(AppError::new(
                ErrorCode::OutputLimit,
                "O grafo excedeu 256 faixas simultâneas.",
            ));
        }
        self.tracks.push(None);
        Ok(self.tracks.len() - 1)
    }
    pub fn push(&mut self, commit: &Commit) -> Result<GraphRow> {
        let lane = match self
            .tracks
            .iter()
            .position(|t| t.as_ref().is_some_and(|t| t.parent == commit.oid))
        {
            Some(i) => i,
            None => self.free()?,
        };
        let mut segments = Vec::with_capacity(
            self.tracks.iter().filter(|t| t.is_some()).count() + commit.parents.len(),
        );
        for (i, track) in self.tracks.iter_mut().enumerate() {
            if let Some(t) = track {
                let arrives = t.parent == commit.oid;
                segments.push(GraphSegment {
                    child: t.child.clone(),
                    parent: t.parent.clone(),
                    from_lane: i as u16,
                    to_lane: if arrives { lane as u16 } else { i as u16 },
                    from_y: 0,
                    to_y: if arrives { 1 } else { 2 },
                    color: t.color,
                });
                if arrives {
                    *track = None;
                }
            }
        }
        for (i, parent) in commit.parents.iter().enumerate() {
            let target = if i == 0 { lane } else { self.free()? };
            let color = if i == 0 { lane as u16 } else { target as u16 };
            segments.push(GraphSegment {
                child: commit.oid.clone(),
                parent: parent.clone(),
                from_lane: lane as u16,
                to_lane: target as u16,
                from_y: 1,
                to_y: 2,
                color,
            });
            self.tracks[target] = Some(Track {
                child: commit.oid.clone(),
                parent: parent.clone(),
                color,
            });
        }
        let row = GraphRow {
            oid: commit.oid.clone(),
            index: self.index,
            lane: lane as u16,
            width: self.tracks.len() as u16,
            segments,
            refs: Vec::new(),
        };
        self.index = self.index.checked_add(1).ok_or_else(AppError::protocol)?;
        Ok(row)
    }
}
