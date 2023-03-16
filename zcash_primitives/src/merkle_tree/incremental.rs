//! Implementations of serialization and parsing for Orchard note commitment trees.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};

use bridgetree::{Frontier, MerkleBridge, NonEmptyFrontier};
use incrementalmerkletree::{Address, Hashable, Level, Position};
use orchard::tree::MerkleHashOrchard;
use zcash_encoding::{Optional, Vector};

use super::{CommitmentTree, HashSer};
use crate::sapling;

pub const SER_V1: u8 = 1;
pub const SER_V2: u8 = 2;

impl HashSer for MerkleHashOrchard {
    fn read<R: Read>(mut reader: R) -> io::Result<Self>
    where
        Self: Sized,
    {
        let mut repr = [0u8; 32];
        reader.read_exact(&mut repr)?;
        <Option<_>>::from(Self::from_bytes(&repr)).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Non-canonical encoding of Pallas base field value.",
            )
        })
    }

    fn write<W: Write>(&self, mut writer: W) -> io::Result<()> {
        writer.write_all(&self.to_bytes())
    }
}

/// Writes a usize value encoded as a u64 in little-endian order. Since usize
/// is platform-dependent, we consistently represent it as u64 in serialized
/// formats.
pub fn write_usize_leu64<W: Write>(mut writer: W, value: usize) -> io::Result<()> {
    // Panic if we get a usize value that can't fit into a u64.
    writer.write_u64::<LittleEndian>(value.try_into().unwrap())
}

/// Reads a usize value encoded as a u64 in little-endian order. Since usize
/// is platform-dependent, we consistently represent it as u64 in serialized
/// formats.
pub fn read_leu64_usize<R: Read>(mut reader: R) -> io::Result<usize> {
    reader.read_u64::<LittleEndian>()?.try_into().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "usize could not be decoded from a 64-bit value on this platform: {:?}",
                e
            ),
        )
    })
}

pub fn write_position<W: Write>(mut writer: W, position: Position) -> io::Result<()> {
    write_usize_leu64(&mut writer, position.into())
}

pub fn read_position<R: Read>(mut reader: R) -> io::Result<Position> {
    read_leu64_usize(&mut reader).map(Position::from)
}

pub fn write_address<W: Write>(mut writer: W, addr: Address) -> io::Result<()> {
    writer.write_u8(addr.level().into())?;
    write_usize_leu64(&mut writer, addr.index())
}

pub fn read_address<R: Read>(mut reader: R) -> io::Result<Address> {
    let level = reader.read_u8().map(Level::from)?;
    let index = read_leu64_usize(&mut reader)?;
    Ok(Address::from_parts(level, index))
}

pub fn read_frontier_v0<H: Hashable + HashSer + Clone, R: Read>(
    mut reader: R,
) -> io::Result<Frontier<H, { sapling::NOTE_COMMITMENT_TREE_DEPTH }>> {
    let tree = CommitmentTree::<H, { sapling::NOTE_COMMITMENT_TREE_DEPTH }>::read(&mut reader)?;

    Ok(tree.to_frontier())
}

pub fn write_nonempty_frontier_v1<H: HashSer, W: Write>(
    mut writer: W,
    frontier: &NonEmptyFrontier<H>,
) -> io::Result<()> {
    write_position(&mut writer, frontier.position())?;
    if frontier.position().is_odd() {
        // The v1 serialization wrote the sibling of a right-hand leaf as a non-optional value,
        // rather than as part of the ommers vector.
        frontier
            .ommers()
            .get(0)
            .expect("ommers vector cannot be empty for right-hand nodes")
            .write(&mut writer)?;
        Optional::write(&mut writer, Some(frontier.leaf()), |w, n: &H| n.write(w))?;
        Vector::write(&mut writer, &frontier.ommers()[1..], |w, e| e.write(w))?;
    } else {
        frontier.leaf().write(&mut writer)?;
        Optional::write(&mut writer, None, |w, n: &H| n.write(w))?;
        Vector::write(&mut writer, frontier.ommers(), |w, e| e.write(w))?;
    }

    Ok(())
}

#[allow(clippy::redundant_closure)]
pub fn read_nonempty_frontier_v1<H: HashSer + Clone, R: Read>(
    mut reader: R,
) -> io::Result<NonEmptyFrontier<H>> {
    let position = read_position(&mut reader)?;
    let left = H::read(&mut reader)?;
    let right = Optional::read(&mut reader, H::read)?;
    let mut ommers = Vector::read(&mut reader, |r| H::read(r))?;

    let leaf = if let Some(right) = right {
        // if the frontier has a right leaf, then the left leaf is the first ommer
        ommers.insert(0, left);
        right
    } else {
        left
    };

    NonEmptyFrontier::from_parts(position, leaf, ommers).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Parsing resulted in an invalid Merkle frontier: {:?}", err),
        )
    })
}

pub fn write_frontier_v1<H: HashSer, W: Write>(
    writer: W,
    frontier: &Frontier<H, 32>,
) -> io::Result<()> {
    Optional::write(writer, frontier.value(), write_nonempty_frontier_v1)
}

#[allow(clippy::redundant_closure)]
pub fn read_frontier_v1<H: HashSer + Clone, R: Read>(reader: R) -> io::Result<Frontier<H, 32>> {
    match Optional::read(reader, read_nonempty_frontier_v1)? {
        None => Ok(Frontier::empty()),
        Some(f) => Frontier::try_from(f).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Parsing resulted in an invalid Merkle frontier: {:?}", err),
            )
        }),
    }
}

// pub fn write_auth_fragment_v1<H: HashSer, W: Write>(
//     mut writer: W,
//     fragment: &AuthFragment<H>,
// ) -> io::Result<()> {
//     write_position(&mut writer, fragment.position())?;
//     write_usize_leu64(&mut writer, fragment.altitudes_observed())?;
//     Vector::write(&mut writer, fragment.values(), |w, a| a.write(w))
// }

#[allow(clippy::redundant_closure)]
pub fn read_auth_fragment_v1<H: HashSer, R: Read>(
    mut reader: R,
) -> io::Result<(Position, usize, Vec<H>)> {
    let position = read_position(&mut reader)?;
    let alts_observed = read_leu64_usize(&mut reader)?;
    let values = Vector::read(&mut reader, |r| H::read(r))?;

    Ok((position, alts_observed, values))
}

pub fn write_bridge_v2<H: HashSer + Ord, W: Write>(
    mut writer: W,
    bridge: &MerkleBridge<H>,
) -> io::Result<()> {
    Optional::write(&mut writer, bridge.prior_position(), |w, pos| {
        write_position(w, pos)
    })?;
    Vector::write_sized(&mut writer, bridge.tracking().iter(), |mut w, addr| {
        write_address(&mut w, *addr)
    })?;
    Vector::write_sized(
        &mut writer,
        bridge.ommers().iter(),
        |mut w, (addr, value)| {
            write_address(&mut w, *addr)?;
            value.write(&mut w)
        },
    )?;
    write_nonempty_frontier_v1(&mut writer, bridge.frontier())?;

    Ok(())
}

pub fn read_bridge_v1<H: HashSer + Ord + Clone, R: Read>(
    mut reader: R,
) -> io::Result<MerkleBridge<H>> {
    fn levels_required(pos: Position) -> impl Iterator<Item = Level> {
        (0u8..64).into_iter().filter_map(move |i| {
            if usize::from(pos) == 0 || usize::from(pos) & (1 << i) == 0 {
                Some(Level::from(i))
            } else {
                None
            }
        })
    }

    let prior_position = Optional::read(&mut reader, read_position)?;

    let fragments = Vector::read(&mut reader, |mut r| {
        let fragment_position = read_position(&mut r)?;
        let (pos, levels_observed, values) = read_auth_fragment_v1(r)?;

        if fragment_position == pos {
            Ok((pos, levels_observed, values))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Auth fragment position mismatch: {:?} != {:?}",
                    fragment_position, pos
                ),
            ))
        }
    })?;

    let frontier = read_nonempty_frontier_v1(&mut reader)?;

    let mut tracking = BTreeSet::new();
    let mut ommers = BTreeMap::new();
    for (pos, levels_observed, values) in fragments.into_iter() {
        // get the list of levels at which we expect to find future ommers for the position being
        // tracked
        let levels = levels_required(pos)
            .take(levels_observed + 1)
            .collect::<Vec<_>>();

        // track the currently-incomplete parent of the tracked position at max height (the one
        // we're currently building)
        tracking.insert(Address::above_position(*levels.last().unwrap(), pos));

        for (level, ommer_value) in levels
            .into_iter()
            .rev()
            .skip(1)
            .zip(values.into_iter().rev())
        {
            let ommer_address = Address::above_position(level, pos).sibling();
            ommers.insert(ommer_address, ommer_value);
        }
    }

    Ok(MerkleBridge::from_parts(
        prior_position,
        tracking,
        ommers,
        frontier,
    ))
}

pub fn read_bridge_v2<H: HashSer + Ord + Clone, R: Read>(
    mut reader: R,
) -> io::Result<MerkleBridge<H>> {
    let prior_position = Optional::read(&mut reader, read_position)?;
    let tracking = Vector::read_collected(&mut reader, |mut r| read_address(&mut r))?;
    let ommers = Vector::read_collected(&mut reader, |mut r| {
        let addr = read_address(&mut r)?;
        let value = H::read(&mut r)?;
        Ok((addr, value))
    })?;
    let frontier = read_nonempty_frontier_v1(&mut reader)?;

    Ok(MerkleBridge::from_parts(
        prior_position,
        tracking,
        ommers,
        frontier,
    ))
}

pub fn write_bridge<H: HashSer + Ord, W: Write>(
    mut writer: W,
    bridge: &MerkleBridge<H>,
) -> io::Result<()> {
    writer.write_u8(SER_V2)?;
    write_bridge_v2(writer, bridge)
}

pub fn read_bridge<H: HashSer + Ord + Clone, R: Read>(
    mut reader: R,
) -> io::Result<MerkleBridge<H>> {
    match reader.read_u8()? {
        SER_V1 => read_bridge_v1(&mut reader),
        SER_V2 => read_bridge_v2(&mut reader),
        flag => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Unrecognized serialization version: {:?}", flag),
        )),
    }
}

#[cfg(test)]
mod tests {
    use bridgetree::{BridgeTree, Frontier};
    use hex;
    use proptest::prelude::*;

    use super::*;
    use crate::{
        merkle_tree::testing::{arb_commitment_tree, TestNode},
        sapling::{testing as sapling, Node},
    };

    proptest! {
        #[test]
        fn frontier_serialization_v0(t in arb_commitment_tree::<_, _, 32>(0, sapling::arb_node()))
        {
            let mut buffer = vec![];
            t.write(&mut buffer).unwrap();
            let frontier: Frontier<Node, 32> = read_frontier_v0(&buffer[..]).unwrap();

            let expected: Frontier<Node, 32> = t.to_frontier();
            assert_eq!(frontier, expected);
        }

        #[test]
        fn frontier_serialization_v1(t in arb_commitment_tree::<_, _, 32>(1, sapling::arb_node()))
        {
            let original: Frontier<Node, 32> = t.to_frontier();

            let mut buffer = vec![];
            write_frontier_v1(&mut buffer, &original).unwrap();
            let read: Frontier<Node, 32> = read_frontier_v1(&buffer[..]).unwrap();

            assert_eq!(read, original);
        }
    }

    #[test]
    fn test_bridge_roundtrip() {
        let mut t: BridgeTree<TestNode, usize, 8> = BridgeTree::new(10, 0);
        let mut to_unwitness = vec![];
        let mut has_auth_path = vec![];
        for i in 0usize..100 {
            assert!(
                t.append(TestNode(i.try_into().unwrap())),
                "Append should succeed."
            );
            if i % 7 == 0 {
                t.mark();
                if i > 0 && i % 2 == 0 {
                    to_unwitness.push(Position::from(i));
                } else {
                    has_auth_path.push(Position::from(i));
                }
            }
            if i % 5 == 0 {
                t.checkpoint(i + 1);
            }
            if i % 11 == 0 && !to_unwitness.is_empty() {
                let pos = to_unwitness.remove(0);
                t.remove_mark(pos);
            }
        }

        for (i, b) in t
            .prior_bridges()
            .iter()
            .chain(t.current_bridge().iter())
            .enumerate()
        {
            let mut buffer = vec![];
            write_bridge(&mut buffer, b).unwrap();
            let b0 = read_bridge(&buffer[..]).unwrap();
            assert_eq!(b, &b0);

            let buffer2 = hex::decode(&BRIDGE_V1_VECTORS[i]).unwrap();
            let b2 = read_bridge(&buffer2[..]).unwrap();
            assert_eq!(b.prior_position(), b2.prior_position());
            assert_eq!(b.frontier(), b2.frontier());
            // Due to the changed nature of garbage collection, bridgetree-v0.2.0 and later
            // MerkleBridge values may track elements that incrementalmerkletree-v0.3.0 bridges did
            // not; in the case that we remove the mark on a leaf, the ommers being tracked related
            // to that mark may be retained until the next garbage collection pass. Therefore, we
            // can only verify that the legacy tracking set is fully contained in the new tracking
            // set.
            assert!(b.tracking().is_superset(b2.tracking()));
            for (k, v) in b2.ommers() {
                assert_eq!(b.ommers().get(k), Some(v));
            }
        }

        for (pos, witness) in BRIDGE_V1_WITNESSES {
            let path = t.witness(Position::from(*pos), 0).unwrap();
            assert_eq!(witness.to_vec(), path);
        }
    }

    const BRIDGE_V1_VECTORS: &[&str] = &[
        "010000000000000000000000000000000000000000",
        "01010000000000000000010000000000000000000000000000000002000000000000000201000000000000000cf29c71c9b7c4a50500000000000000040000000000000001050000000000000001545227c621102b3a",
        "010105000000000000000100000000000000000000000000000000030000000000000001948e8c8cb96eed280700000000000000060000000000000001070000000000000002bae878935340beb7545227c621102b3a",
        "010107000000000000000200000000000000000000000000000000030000000000000000070000000000000007000000000000000000000000000000000a000000000000000a000000000000000002157a46a02f6b8e34c730d7a51e3a8378",
        "01010a000000000000000200000000000000000000000000000000030000000000000000070000000000000007000000000000000000000000000000000e000000000000000e000000000000000003779388f05d9525bb33fac4dcb323ec8bc730d7a51e3a8378",
        "01010e000000000000000300000000000000000000000000000000040000000000000001c49580ae5e5e758307000000000000000700000000000000010000000000000001c49580ae5e5e75830e000000000000000e000000000000000100000000000000010f000000000000000f000000000000000e00000000000000010f0000000000000003779388f05d9525bb33fac4dcb323ec8bc730d7a51e3a8378",
        "01010f000000000000000300000000000000000000000000000000040000000000000000070000000000000007000000000000000100000000000000000e000000000000000e0000000000000001000000000000000014000000000000001400000000000000000281be7679141b8edda519e57e99af06e2",
        "010114000000000000000300000000000000000000000000000000040000000000000000070000000000000007000000000000000100000000000000000e000000000000000e00000000000000010000000000000000150000000000000014000000000000000115000000000000000281be7679141b8edda519e57e99af06e2",
        "0101150000000000000003000000000000000000000000000000000400000000000000000700000000000000070000000000000001000000000000000015000000000000001500000000000000010000000000000001fdf772cabf2d5b20190000000000000018000000000000000119000000000000000244365e47c1afcfa1a519e57e99af06e2",
        "01011900000000000000030000000000000000000000000000000004000000000000000007000000000000000700000000000000010000000000000000150000000000000015000000000000000100000000000000001c000000000000001c000000000000000003174b903560b67a6744365e47c1afcfa1a519e57e99af06e2",
        "01011c00000000000000040000000000000000000000000000000004000000000000000007000000000000000700000000000000010000000000000000150000000000000015000000000000000100000000000000001c000000000000001c000000000000000100000000000000011d000000000000001e000000000000001e000000000000000004cfe7a4133f88607d174b903560b67a6744365e47c1afcfa1a519e57e99af06e2",
        "01011e00000000000000030000000000000000000000000000000005000000000000000175938fb118b4555d0700000000000000070000000000000002000000000000000175938fb118b4555d15000000000000001500000000000000020000000000000001c050bf17c144495d2300000000000000220000000000000001230000000000000002f22dbb4a83f3b28318bd1c4cf1fd912e",
        "0101230000000000000004000000000000000000000000000000000500000000000000000700000000000000070000000000000002000000000000000015000000000000001500000000000000020000000000000000230000000000000023000000000000000100000000000000017cf828deead5aa3d280000000000000028000000000000000002e28c91f60fbf69d518bd1c4cf1fd912e",
        "0101280000000000000004000000000000000000000000000000000500000000000000000700000000000000070000000000000002000000000000000015000000000000001500000000000000020000000000000000230000000000000023000000000000000100000000000000002a000000000000002a000000000000000003e16610644840a918e28c91f60fbf69d518bd1c4cf1fd912e",
        "01012a0000000000000004000000000000000000000000000000000500000000000000000700000000000000070000000000000002000000000000000015000000000000001500000000000000020000000000000000230000000000000023000000000000000100000000000000002d000000000000002c00000000000000012d0000000000000003afd0751401aff593e28c91f60fbf69d518bd1c4cf1fd912e",
        "01012d000000000000000400000000000000000000000000000000050000000000000000070000000000000007000000000000000200000000000000001500000000000000150000000000000002000000000000000023000000000000002300000000000000020000000000000001dd486e048f1e6c2131000000000000003000000000000000013100000000000000029f16b2397db7491d18bd1c4cf1fd912e",
        "0101310000000000000005000000000000000000000000000000000500000000000000000700000000000000070000000000000002000000000000000015000000000000001500000000000000020000000000000000230000000000000023000000000000000200000000000000003100000000000000310000000000000000000000000000000032000000000000003200000000000000000356370c5489a364b29f16b2397db7491d18bd1c4cf1fd912e",
        "010132000000000000000500000000000000000000000000000000050000000000000000070000000000000007000000000000000200000000000000001500000000000000150000000000000002000000000000000023000000000000002300000000000000020000000000000000310000000000000031000000000000000200000000000000024f6ce1b1025daa00c2db272cc44c77c6370000000000000036000000000000000137000000000000000491c03add43411617ff8c00dfd0bc84289f16b2397db7491d18bd1c4cf1fd912e",
        "0101370000000000000005000000000000000000000000000000000500000000000000000700000000000000070000000000000002000000000000000015000000000000001500000000000000020000000000000000230000000000000023000000000000000200000000000000003100000000000000310000000000000002000000000000000038000000000000003800000000000000000335cff7ed860ba07e9f16b2397db7491d18bd1c4cf1fd912e",
        "010138000000000000000600000000000000000000000000000000050000000000000000070000000000000007000000000000000200000000000000001500000000000000150000000000000002000000000000000023000000000000002300000000000000020000000000000000310000000000000031000000000000000200000000000000003800000000000000380000000000000002000000000000000239000000000000001f6951d61f683ea73c000000000000003c0000000000000000047cfc0f73b07bd1ef35cff7ed860ba07e9f16b2397db7491d18bd1c4cf1fd912e",
        "01013c000000000000000600000000000000000000000000000000060000000000000001f788446318d5f99907000000000000000700000000000000030000000000000001f788446318d5f99915000000000000001500000000000000030000000000000001f788446318d5f99923000000000000002300000000000000030000000000000001fb8290b35ee7b7dd31000000000000003100000000000000030000000000000001ea30e73bc03ac3c638000000000000003800000000000000030000000000000001852e67bafeb639803f000000000000003e00000000000000013f00000000000000050a984f21271cd02d7cfc0f73b07bd1ef35cff7ed860ba07e9f16b2397db7491d18bd1c4cf1fd912e",
        "01013f00000000000000070000000000000000000000000000000006000000000000000007000000000000000700000000000000030000000000000000150000000000000015000000000000000300000000000000002300000000000000230000000000000003000000000000000031000000000000003100000000000000030000000000000000380000000000000038000000000000000300000000000000003f000000000000003f000000000000000000000000000000004100000000000000400000000000000001410000000000000001af35cc5f4335f3a7",
        "010141000000000000000600000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f0000000000000000000000000000000046000000000000004600000000000000000331fc52e580f35e5348d31a06aa5a4470af35cc5f4335f3a7",
        "010146000000000000000700000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004600000000000000460000000000000001000000000000000147000000000000004b000000000000004a00000000000000014b0000000000000003fe1c911e7cebb841b8b5aeae9a67d003af35cc5f4335f3a7",
        "01014b000000000000000700000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f00000000000000000000000000000000460000000000000046000000000000000100000000000000004d000000000000004c00000000000000014d000000000000000347a60f231e2896ceb8b5aeae9a67d003af35cc5f4335f3a7",
        "01014d000000000000000700000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d000000000000000100000000000000011387f5de1c3253dd500000000000000050000000000000000002428877913fee798baf35cc5f4335f3a7",
        "010150000000000000000700000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d00000000000000010000000000000000540000000000000054000000000000000003e22700e31d13d582428877913fee798baf35cc5f4335f3a7",
        "010154000000000000000800000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d000000000000000100000000000000005400000000000000540000000000000001000000000000000155000000000000005500000000000000540000000000000001550000000000000003e22700e31d13d582428877913fee798baf35cc5f4335f3a7",
        "010155000000000000000700000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d000000000000000100000000000000005a000000000000005a00000000000000000498e2ef95e82f2d3e901d6a966885931b428877913fee798baf35cc5f4335f3a7",
        "01015a000000000000000700000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d000000000000000100000000000000005b000000000000005a00000000000000015b000000000000000498e2ef95e82f2d3e901d6a966885931b428877913fee798baf35cc5f4335f3a7",
        "01015b000000000000000800000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d00000000000000020000000000000001e7e0fcdfe98c29685b000000000000005b00000000000000010000000000000001d8900fcea5e695a55f000000000000005e00000000000000015f0000000000000005c3c19bd287343ee12218736715dbc702901d6a966885931b428877913fee798baf35cc5f4335f3a7",
        "01015f000000000000000800000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d000000000000000200000000000000005b000000000000005b000000000000000100000000000000006200000000000000620000000000000000036aa28f62b4a6071a082530c7c14f49acaf35cc5f4335f3a7",
        "010162000000000000000800000000000000000000000000000000060000000000000000070000000000000007000000000000000300000000000000001500000000000000150000000000000003000000000000000023000000000000002300000000000000030000000000000000310000000000000031000000000000000300000000000000003f000000000000003f000000000000000000000000000000004d000000000000004d000000000000000200000000000000005b000000000000005b0000000000000001000000000000000063000000000000006200000000000000016300000000000000036aa28f62b4a6071a082530c7c14f49acaf35cc5f4335f3a7",
    ];

    const BRIDGE_V1_WITNESSES: &[(usize, [TestNode; 8])] = &[
        (
            0,
            [
                TestNode(1),
                TestNode(11944874187515818508),
                TestNode(2949135074203569812),
                TestNode(9472581151991305668),
                TestNode(6725479636698895221),
                TestNode(11095133457725294839),
                TestNode(14133087432678894597),
                TestNode(5741638299150577022),
            ],
        ),
        (
            7,
            [
                TestNode(6),
                TestNode(13240090682216474810),
                TestNode(4191461615442809428),
                TestNode(9472581151991305668),
                TestNode(6725479636698895221),
                TestNode(11095133457725294839),
                TestNode(14133087432678894597),
                TestNode(5741638299150577022),
            ],
        ),
        (
            21,
            [
                TestNode(20),
                TestNode(2331507533852899325),
                TestNode(15964727503826108033),
                TestNode(6721979514944966848),
                TestNode(16286898176225778085),
                TestNode(11095133457725294839),
                TestNode(14133087432678894597),
                TestNode(5741638299150577022),
            ],
        ),
        (
            35,
            [
                TestNode(34),
                TestNode(9489915110043102706),
                TestNode(4443599187080706172),
                TestNode(2408333500339865821),
                TestNode(15976492597045658363),
                TestNode(3355742410173627672),
                TestNode(14133087432678894597),
                TestNode(5741638299150577022),
            ],
        ),
        (
            49,
            [
                TestNode(48),
                TestNode(47953012196469839),
                TestNode(14300983547176410050),
                TestNode(14322355837281448170),
                TestNode(2110419648866555551),
                TestNode(3355742410173627672),
                TestNode(14133087432678894597),
                TestNode(5741638299150577022),
            ],
        ),
        (
            63,
            [
                TestNode(62),
                TestNode(3301169481250740234),
                TestNode(17280729242972191868),
                TestNode(9124305519198588725),
                TestNode(2110419648866555551),
                TestNode(3355742410173627672),
                TestNode(14133087432678894597),
                TestNode(5741638299150577022),
            ],
        ),
        (
            77,
            [
                TestNode(76),
                TestNode(15948145805030164243),
                TestNode(14886129728222111303),
                TestNode(274833491322910136),
                TestNode(7505685190102802663),
                TestNode(2256868868291095598),
                TestNode(12102075187160954287),
                TestNode(5741638299150577022),
            ],
        ),
        (
            91,
            [
                TestNode(90),
                TestNode(4480289880297955992),
                TestNode(11931696387589116120),
                TestNode(1987078544847150480),
                TestNode(10050326000244852802),
                TestNode(2256868868291095598),
                TestNode(12102075187160954287),
                TestNode(5741638299150577022),
            ],
        ),
    ];
}
