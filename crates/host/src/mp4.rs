//! A minimal MP4 writer: one H.264 video track, every sample in one chunk,
//! the index (`moov`) before the data so players can start before the end.
//!
//! Enough for the browser's recordings ([`crate::browser`]); it plays in
//! Safari, AVPlayer, Chromium, Firefox and ffmpeg. Box layouts follow
//! ISO/IEC 14496-12 (the ISO base media file format) and 14496-15 (`avcC`).

/// One encoded picture.
pub struct Sample {
    /// Its NAL units, each prefixed by its length (4 bytes, big-endian),
    /// without parameter sets.
    pub data: Vec<u8>,
    /// How long it shows, in the track's timescale.
    pub duration: u32,
    /// Whether it decodes on its own (an IDR picture).
    pub sync: bool,
}

/// The video's description.
pub struct Track<'a> {
    pub width: u16,
    pub height: u16,
    /// Ticks per second of the sample durations.
    pub timescale: u32,
    /// The sequence and picture parameter sets, without start codes.
    pub sps: &'a [u8],
    pub pps: &'a [u8],
}

/// Split an Annex B stream (what encoders write: `00 00 01`-separated) into
/// its NAL units.
pub fn nal_units(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut out = vec![];
    let mut i = 0;
    let mut start = None;
    while i + 3 <= annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            if let Some(s) = start {
                // A four-byte start code's leading zero belongs to it, not the NAL.
                let mut end = i;
                while end > s && annex_b[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    out.push(&annex_b[s..end]);
                }
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < annex_b.len() {
            out.push(&annex_b[s..]);
        }
    }
    out
}

/// A NAL unit's type (7 SPS, 8 PPS, 5 IDR slice, 1 other slice).
pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|b| b & 0x1f).unwrap_or(0)
}

/// The whole file.
pub fn write(track: &Track, samples: &[Sample]) -> Vec<u8> {
    let ftyp = ftyp();
    // The data's offset depends on the index's size, which doesn't depend on
    // the offset's value: build it once to measure, then for real.
    let probe = moov(track, samples, 0);
    let data_at = (ftyp.len() + probe.len() + 8) as u32;
    let moov = moov(track, samples, data_at);
    debug_assert_eq!(moov.len(), probe.len());
    let data_len: usize = samples.iter().map(|s| s.data.len()).sum();
    let mut out = Vec::with_capacity(ftyp.len() + moov.len() + 8 + data_len);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&((data_len + 8) as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for s in samples {
        out.extend_from_slice(&s.data);
    }
    out
}

/// A box: its size, its type, its contents.
fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let len: usize = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut b = Vec::with_capacity(len);
    b.extend_from_slice(&(len as u32).to_be_bytes());
    b.extend_from_slice(kind);
    for p in parts {
        b.extend_from_slice(p);
    }
    b
}

/// A full box: a box that starts with a version and flags.
fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let vf = ((version as u32) << 24 | (flags & 0x00ff_ffff)).to_be_bytes();
    bx(kind, &[&vf, body])
}

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    /// The identity transform.
    fn matrix(&mut self) -> &mut Self {
        for v in [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000u32] {
            self.u32(v);
        }
        self
    }
}

fn ftyp() -> Vec<u8> {
    bx(b"ftyp", &[b"isom", &512u32.to_be_bytes(), b"isom", b"iso2", b"avc1", b"mp41"])
}

fn moov(t: &Track, samples: &[Sample], data_at: u32) -> Vec<u8> {
    let duration: u64 = samples.iter().map(|s| s.duration as u64).sum();
    let duration = duration.min(u32::MAX as u64) as u32;

    let mvhd = full(b"mvhd", 0, 0, &W::default().u32(0).u32(0).u32(t.timescale).u32(duration).u32(0x0001_0000).u16(0x0100).zeros(10).matrix().zeros(24).u32(2).0);
    // Enabled, in the movie, in its preview.
    let tkhd = full(b"tkhd", 0, 0x7, &W::default().u32(0).u32(0).u32(1).u32(0).u32(duration).zeros(8).u16(0).u16(0).u16(0).u16(0).matrix().u32((t.width as u32) << 16).u32((t.height as u32) << 16).0);
    // Language "und".
    let mdhd = full(b"mdhd", 0, 0, &W::default().u32(0).u32(0).u32(t.timescale).u32(duration).u16(0x55c4).u16(0).0);
    let hdlr = full(b"hdlr", 0, 0, &W::default().u32(0).bytes(b"vide").zeros(12).bytes(b"VideoHandler\0").0);
    let vmhd = full(b"vmhd", 0, 1, &[0u8; 8]);
    let dref = full(b"dref", 0, 0, &W::default().u32(1).bytes(&full(b"url ", 0, 1, &[])).0);
    let dinf = bx(b"dinf", &[&dref]);

    let avcc = {
        let mut w = W::default();
        w.u8(1).u8(t.sps.get(1).copied().unwrap_or(66)).u8(t.sps.get(2).copied().unwrap_or(0)).u8(t.sps.get(3).copied().unwrap_or(31));
        // Four-byte lengths; one SPS, one PPS.
        w.u8(0xff).u8(0xe1).u16(t.sps.len() as u16).bytes(t.sps).u8(1).u16(t.pps.len() as u16).bytes(t.pps);
        bx(b"avcC", &[&w.0])
    };
    let avc1 = {
        let mut w = W::default();
        w.zeros(6).u16(1).zeros(16).u16(t.width).u16(t.height).u32(0x0048_0000).u32(0x0048_0000).u32(0).u16(1).zeros(32).u16(0x0018).u16(0xffff);
        w.bytes(&avcc);
        bx(b"avc1", &[&w.0])
    };
    let stsd = full(b"stsd", 0, 0, &W::default().u32(1).bytes(&avc1).0);

    // Durations, run-length coded.
    let mut runs: Vec<(u32, u32)> = vec![];
    for s in samples {
        match runs.last_mut() {
            Some((n, d)) if *d == s.duration => *n += 1,
            _ => runs.push((1, s.duration)),
        }
    }
    let mut w = W::default();
    w.u32(runs.len() as u32);
    for (n, d) in runs {
        w.u32(n).u32(d);
    }
    let stts = full(b"stts", 0, 0, &w.0);

    let syncs: Vec<u32> = samples.iter().enumerate().filter(|(_, s)| s.sync).map(|(i, _)| i as u32 + 1).collect();
    let mut w = W::default();
    w.u32(syncs.len() as u32);
    for n in &syncs {
        w.u32(*n);
    }
    let stss = full(b"stss", 0, 0, &w.0);

    let stsc = full(b"stsc", 0, 0, &W::default().u32(1).u32(1).u32(samples.len() as u32).u32(1).0);
    let mut w = W::default();
    w.u32(0).u32(samples.len() as u32);
    for s in samples {
        w.u32(s.data.len() as u32);
    }
    let stsz = full(b"stsz", 0, 0, &w.0);
    let stco = full(b"stco", 0, 0, &W::default().u32(1).u32(data_at).0);

    let stbl = bx(b"stbl", &[&stsd, &stts, &stss, &stsc, &stsz, &stco]);
    let minf = bx(b"minf", &[&vmhd, &dinf, &stbl]);
    let mdia = bx(b"mdia", &[&mdhd, &hdlr, &minf]);
    let trak = bx(b"trak", &[&tkhd, &mdia]);
    bx(b"moov", &[&mvhd, &trak])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_on_either_start_code() {
        let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let n = nal_units(&s);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
        assert_eq!(n.iter().map(|x| nal_type(x)).collect::<Vec<_>>(), vec![7, 8, 5]);
        assert!(nal_units(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn the_index_comes_first_and_points_at_the_data() {
        let samples = vec![
            Sample { data: vec![0, 0, 0, 2, 0x65, 9], duration: 100, sync: true },
            Sample { data: vec![0, 0, 0, 1, 0x41], duration: 100, sync: false },
            Sample { data: vec![0, 0, 0, 1, 0x41], duration: 250, sync: false },
        ];
        let f = write(&Track { width: 64, height: 48, timescale: 1000, sps: &[0x67, 0x42, 0xc0, 0x1f], pps: &[0x68, 0xce] }, &samples);
        assert_eq!(&f[4..8], b"ftyp");
        let ftyp_len = u32::from_be_bytes(f[0..4].try_into().unwrap()) as usize;
        assert_eq!(&f[ftyp_len + 4..ftyp_len + 8], b"moov");
        let moov_len = u32::from_be_bytes(f[ftyp_len..ftyp_len + 4].try_into().unwrap()) as usize;
        let mdat = ftyp_len + moov_len;
        assert_eq!(&f[mdat + 4..mdat + 8], b"mdat");
        assert_eq!(&f[mdat + 8..mdat + 14], &[0, 0, 0, 2, 0x65, 9]);
        // stco names where the samples start.
        let at = f.windows(4).position(|w| w == b"stco").unwrap();
        let off = u32::from_be_bytes(f[at + 12..at + 16].try_into().unwrap()) as usize;
        assert_eq!(off, mdat + 8);
        // Two runs of durations: 2×100, 1×250; total 450.
        let at = f.windows(4).position(|w| w == b"stts").unwrap();
        assert_eq!(u32::from_be_bytes(f[at + 8..at + 12].try_into().unwrap()), 2);
        let at = f.windows(4).position(|w| w == b"mvhd").unwrap();
        assert_eq!(u32::from_be_bytes(f[at + 20..at + 24].try_into().unwrap()), 450);
    }
}
