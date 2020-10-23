use byteorder::{ByteOrder, LittleEndian};
use etherparse::{ReadError, SlicedPacket, TransportSlice};
use log::*;
use pcap_file::{pcap::PcapReader, PcapError};
use snafu::{ensure, ResultExt, Snafu};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to parse pcap file: {}", source))]
    Pcap { source: PcapError },
    #[snafu(display(
        "Failed to parse ethernet frame: {:?}; packet: {:x?}",
        read_error,
        packet
    ))]
    EtherParse {
        read_error: ReadError,
        packet: Vec<u8>,
    },
    #[snafu(display(
        "Malformed UDP packet (invalid length or missing \"ixy\"): {:x?}",
        packet
    ))]
    MalformedUdpPacket { packet: Vec<u8> },
    #[snafu(display("Incorrect packet count: expected: {} actual: {}", expected, actual))]
    IncorrectPacketCount { expected: usize, actual: usize },
    #[snafu(display(
        "Bad sequence number: expected {} packets but max sequence number was {}",
        packets,
        max_seq_num
    ))]
    BadSequenceNumber { packets: usize, max_seq_num: u32 },
    #[snafu(display("Some sequence number occured more than once"))]
    DuplicateSequenceNumber,
    // #[snafu(display("Wrong packet order: last sequence number was {} and now encountered {}", last_seq_num, seq_num))]
    // InvalidSequenceOrder {
    //     last_seq_num: u32,
    //     seq_num: u32,
    // },
}

pub fn test_pcap(pcap: &[u8], pcap_n: usize) -> Result<(), Error> {
    // TODO: Check that no packets are duplicated
    let pcap_reader = PcapReader::new(pcap).context(Pcap)?;

    let mut count = 0;
    // let mut last_seq_num = None;
    let mut max_seq_num = 0;
    let mut seq_nums = Vec::new();
    for pcap in pcap_reader {
        let pcap = pcap.context(Pcap)?;
        let packet = SlicedPacket::from_ethernet(&pcap.data).map_err(|e| Error::EtherParse {
            read_error: e,
            packet: pcap.data.to_vec(),
        })?;

        if let Some(TransportSlice::Udp(udp_header)) = packet.transport {
            if udp_header.length() != 26 || packet.payload[..3] != *b"ixy" {
                return Err(Error::MalformedUdpPacket {
                    packet: pcap.data.to_vec(),
                });
            }
            let len = packet.payload.len();
            let seq_num = LittleEndian::read_u32(&packet.payload[(len - 4)..]);
            max_seq_num = max_seq_num.max(seq_num);
            seq_nums.push(seq_num);
            // Currently disabled as there's some kind of packet reordering happening on OpenStack
            // Using the local libvirt/qemu setup no packets are reordered
            // Remove redundant duplicate packet check again after reenabling this
            // if let Some(last_seq_num) = last_seq_num {
            //     if seq_num <= last_seq_num {
            //         return Err(Error::InvalidSequenceOrder {
            //             last_seq_num,
            //             seq_num,
            //         });
            //     }
            // }
            // last_seq_num = Some(seq_num);
            count += 1;
        } else {
            debug!("ignoring non-UDP packet")
        }
    }

    // Check that packet count is correct and that we didn't drop too many packets
    ensure!(
        count == pcap_n,
        IncorrectPacketCount {
            expected: pcap_n,
            actual: count
        }
    );
    ensure!(
        max_seq_num as usize >= pcap_n - 1 && max_seq_num as usize <= pcap_n * 2,
        BadSequenceNumber {
            packets: pcap_n,
            max_seq_num,
        }
    );

    let pre_dedup = seq_nums.len();
    seq_nums.sort_unstable();
    seq_nums.dedup();
    ensure!(seq_nums.len() == pre_dedup, DuplicateSequenceNumber);

    Ok(())
}
