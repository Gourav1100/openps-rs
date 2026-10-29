//! Minimal Ogg reader for tests and demos: extracts the Opus packets of an `.opus` file (skips OpusHead / OpusTags).

/// Packets of the first logical stream, in order. Returns `None` for anything that is not well-formed Ogg Opus.
pub fn opus_packets(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut packets: Vec<Vec<u8>> = vec![];
    let mut cur: Vec<u8> = vec![];
    let mut i = 0;
    while i < data.len() {
        let page = data.get(i..i + 27)?;
        if &page[..4] != b"OggS" {
            return None;
        }
        let nseg = page[26] as usize;
        let table = data.get(i + 27..i + 27 + nseg)?;
        let mut off = i + 27 + nseg;
        for &lace in table {
            cur.extend(data.get(off..off + lace as usize)?);
            off += lace as usize;
            if lace < 255 {
                packets.push(std::mem::take(&mut cur));
            }
        }
        i = off;
    }
    if packets.len() < 2 || !packets[0].starts_with(b"OpusHead") || !packets[1].starts_with(b"OpusTags") {
        return None;
    }
    packets.drain(..2);
    Some(packets)
}

/// (channels, pre-skip, input sample rate) from the OpusHead packet.
pub fn head(data: &[u8]) -> Option<(u8, u16, u32)> {
    let h = data.get(28..)?; // first page: 27 + 1 lacing byte
    let h = h.strip_prefix(b"OpusHead")?;
    Some((
        *h.get(1)?,
        u16::from_le_bytes(h.get(2..4)?.try_into().ok()?),
        u32::from_le_bytes(h.get(4..8)?.try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(packets: &[&[u8]], header_type: u8) -> Vec<u8> {
        let mut segs = vec![];
        for p in packets {
            let mut n = p.len();
            while n >= 255 {
                segs.push(255u8);
                n -= 255;
            }
            segs.push(n as u8);
        }
        let mut v = b"OggS".to_vec();
        v.extend([0, header_type]);
        v.extend([0u8; 20]); // granule, serial, sequence, crc (not checked)
        v.push(segs.len() as u8);
        v.extend(&segs);
        for p in packets {
            v.extend(*p);
        }
        v
    }

    #[test]
    fn extracts_packets_including_long_ones_and_rejects_garbage() {
        let big = vec![7u8; 600]; // needs 255+255+90 lacing
        let mut f = page(&[b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00"], 2);
        f.extend(page(&[b"OpusTags"], 0));
        f.extend(page(&[&[1, 2, 3], &big, &[]], 0));
        let p = opus_packets(&f).unwrap();
        assert_eq!(p, vec![vec![1, 2, 3], big, vec![]]);
        assert_eq!(head(&f), Some((2, 312, 48000)));
        assert!(opus_packets(b"not ogg at all, definitely not").is_none());
        assert!(opus_packets(&f[..f.len() - 3]).is_none(), "truncated page");
        assert!(opus_packets(&[]).is_none());
        let mut nohead = page(&[b"xxxxxxxx"], 2);
        nohead.extend(page(&[b"yyyyyyyy"], 0));
        assert!(opus_packets(&nohead).is_none());
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut x = 0x1234_5678_9abc_def1u64;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let n = (x % 300) as usize;
            let mut d: Vec<u8> = (0..n).map(|i| (x >> (i % 56)) as u8).collect();
            if n > 4 && x & 1 == 0 {
                d[..4].copy_from_slice(b"OggS");
            }
            let _ = opus_packets(&d);
            let _ = head(&d);
        }
    }
}
