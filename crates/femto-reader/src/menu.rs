//! Shared menu rendering and pointer geometry.
use crate::{Error, Result};

pub(crate) struct Geometry {
    pub rows: usize,
    pub items: usize,
    pub columns: usize,
    width: u32,
    height: u32,
}
impl Geometry {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let rows = (height / 80).min(8) as usize;
        if !(480..=2048).contains(&width) || height > 2048 || rows < 4 {
            return Err(Error::Layout("menu viewport too small or large"));
        }
        let items = rows - 2;
        Ok(Self {
            rows,
            items,
            columns: 36_usize.div_ceil(items),
            width,
            height,
        })
    }
    pub fn keyboard(&self) -> Result<()> {
        if self.width / (self.columns as u32) < 80 {
            return Err(Error::Layout("keyboard viewport too small"));
        }
        Ok(())
    }
    pub fn row(&self, index: usize) -> (u32, u32) {
        bounds(index, self.height, self.rows)
    }
    pub fn row_at(&self, y: u32) -> Option<usize> {
        cell_at(y, self.height, self.rows)
    }
    pub fn key(&self, index: usize) -> (usize, u32, u32) {
        let (row, column, columns) = if index < 36 {
            (index / self.columns + 1, index % self.columns, self.columns)
        } else {
            (self.rows - 1, index - 36, 4)
        };
        let (x, width) = bounds(column, self.width, columns);
        (row, x, width)
    }
    pub fn key_at(&self, x: u32, row: usize) -> Option<usize> {
        if row == 0 || row >= self.rows {
            return None;
        }
        if row == self.rows - 1 {
            return cell_at(x, self.width, 4).map(|c| 36 + c);
        }
        let index = (row - 1) * self.columns + cell_at(x, self.width, self.columns)?;
        (index < 36).then_some(index)
    }
}
fn bounds(index: usize, extent: u32, cells: usize) -> (u32, u32) {
    let start = index as u32 * extent / cells as u32;
    (start, (index as u32 + 1) * extent / cells as u32 - start)
}
fn cell_at(point: u32, extent: u32, cells: usize) -> Option<usize> {
    (point < extent)
        .then(|| (((u64::from(point) + 1) * cells as u64 - 1) / u64::from(extent)) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendering_partitions_and_hit_tests_agree_at_every_pixel() {
        for (width, height) in [(480, 800), (800, 480), (803, 483), (801, 561)] {
            let geometry = Geometry::new(width, height).unwrap();
            geometry.keyboard().unwrap();
            let mut next = 0;
            for row in 0..geometry.rows {
                let (start, size) = geometry.row(row);
                assert_eq!(start, next);
                next += size;
                for y in start..start + size {
                    assert_eq!(geometry.row_at(y), Some(row));
                }
            }
            assert_eq!(next, height);
            assert_eq!(geometry.row_at(height), None);
            for index in 0..40 {
                let (row, start, size) = geometry.key(index);
                for x in start..start + size {
                    assert_eq!(geometry.key_at(x, row), Some(index));
                }
                assert_eq!(geometry.key_at(width, row), None);
            }
        }
    }
}
