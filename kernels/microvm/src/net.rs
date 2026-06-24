//! E6.5 Phase 1 — virtio-net + smoltcp: the kernel's first real network path.
//!
//! Until now every byte the microVM exchanged with the outside world went
//! over COM1. This module brings up a **virtio-net** NIC (modern PCI
//! transport, the device model Cloud Hypervisor and QEMU q35 expose), wires
//! it into the same `smoltcp` stack the E4.3 loopback proved, and carries the
//! operator-console wire protocol (E6 Phase 1) over a real TCP connection:
//!
//! 1. probe the PCI ECAM for a virtio-net function and negotiate the device
//!    (`virtio-drivers`: descriptor rings, feature bits, MAC read-out);
//! 2. bring up `smoltcp` on the QEMU/slirp user-net constants
//!    (`10.0.2.15/24`, gateway `10.0.2.2`) — no DHCP dependency;
//! 3. connect to the host-side console listener at `10.0.2.2:9099`, frame one
//!    `ANIMA_TLM <ndjson>` telemetry line over the socket, and decode the
//!    `ANIMA_IN <ndjson>` guidance line the host answers with via the same
//!    `console_proto` parser the COM1 path uses;
//! 4. on success print `E6.5_NET_DONE` (transport up + telemetry egress) and
//!    `E6.5_GUIDANCE_OK` (afferent ingress decoded) for the CI gate.
//!
//! The phase is **self-skipping**: when no virtio-net function exists (the
//! plain soak invocation, Firecracker's mmio-only board) it reports and
//! returns `Ok` without markers, so existing harnesses are unaffected.
//!
//! # Safety
//!
//! The `Hal` implementation below relies on the UEFI boot-services
//! environment: memory is identity-mapped, so physical and virtual addresses
//! coincide, and the global `BumpAllocator` hands out DMA-safe RAM. Every
//! `unsafe` block is annotated and recorded in `crates/corpus/unsafe_audit.md`.

use alloc::vec;
use alloc::vec::Vec;
use core::ptr::NonNull;

use console_proto::{parse_input_line, OperatorEvent, TELEMETRY_PREFIX};
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use virtio_drivers::device::net::VirtIONet;
use virtio_drivers::transport::pci::bus::{Cam, Command, MmioCam, PciRoot};
use virtio_drivers::transport::pci::{virtio_device_type, PciTransport};
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

/// Fallback ECAM (MMCONFIG) windows, most likely first, used when ACPI MCFG
/// discovery (`acpi::mcfg_ecam_bases`) yields nothing. OVMF on QEMU q35
/// programs 0xE000_0000; SeaBIOS-era q35 and some Cloud Hypervisor builds
/// use 0xB000_0000. A candidate is accepted only if the host bridge at
/// 00:00.0 reads back a real vendor id (not 0x0000/0xFFFF), so a wrong
/// guess degrades to "no device", never to UB. The durable path — reading
/// the real base from ACPI MCFG (docs/22 §1) — is now wired in `probe_virtio_net`
/// via `crate::acpi`; this list remains as the belt-and-braces fallback.
const ECAM_CANDIDATES: &[usize] = &[0xE000_0000, 0xB000_0000];

/// slirp user-net constants (QEMU `-netdev user`): guest address and the
/// host-side gateway alias the console listener binds behind.
const GUEST_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
const HOST_ALIAS: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
const CONSOLE_PORT: u16 = 9099;

/// Virtqueue depth; 16 descriptors is ample for a single bounded exchange.
const QUEUE_SIZE: usize = 16;
/// Per-buffer size: one full Ethernet frame + virtio-net header.
const NET_BUF_LEN: usize = 2048;

// ─── Hal: identity-mapped DMA over the kernel bump allocator ────────────────

struct KernelHal;

// SAFETY: the UEFI boot-services environment identity-maps all RAM, so a
// virtual address handed out by the global allocator *is* its physical
// address, satisfying every Hal contract below: dma_alloc returns
// zero-initialised, page-aligned, never-reused memory (bump allocator);
// mmio_phys_to_virt is the identity; share/unshare need no bounce buffers.
unsafe impl Hal for KernelHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let layout =
            core::alloc::Layout::from_size_align(pages * 4096, 4096).expect("virtio dma layout");
        // SAFETY: layout is non-zero; alloc_zeroed satisfies the trait's
        // "zero-initialised" requirement; the bump allocator never reuses
        // the region, so the device owns it for its whole lifetime.
        let vaddr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(vaddr).expect("virtio dma_alloc: heap exhausted");
        (vaddr as PhysAddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        // Bump allocator: deliberate leak (boot-lifetime device).
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        // SAFETY: identity mapping under UEFI boot services.
        NonNull::new(paddr as *mut u8).expect("virtio mmio at null")
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        // SAFETY: identity mapping — the buffer's virtual address is its
        // physical address; no bounce buffer is required.
        buffer.as_ptr() as *mut u8 as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        // Identity mapping: nothing to copy back or unmap.
    }
}

type Net = VirtIONet<KernelHal, PciTransport, QUEUE_SIZE>;

// ─── PCI probe ───────────────────────────────────────────────────────────────

/// Walk PCI bus 0 through the q35/Cloud-Hypervisor ECAM and bring up the
/// first virtio-net function. Returns `None` when the board has no ECAM
/// mapping or no virtio-net device (the self-skip path).
fn probe_virtio_net(serial: &impl Fn(&str)) -> Option<Net> {
    // Prefer the ECAM base(s) the firmware declares in ACPI MCFG, then fall
    // back to the hard-coded candidates. Each base is still validated below by
    // the host-bridge vendor-id check, so an absent or malformed MCFG only ever
    // degrades to the prior candidate-scan behaviour.
    let mut bases: Vec<usize> = crate::acpi::mcfg_ecam_bases(serial)
        .into_iter()
        .map(|b| b as usize)
        .collect();
    for &fallback in ECAM_CANDIDATES {
        if !bases.contains(&fallback) {
            bases.push(fallback);
        }
    }

    let mut root = None;
    for &base in &bases {
        // SAFETY: each base is a board MMCONFIG window in identity-mapped
        // device space; a wrong base reads 0x0000/0xFFFF vendor ids and
        // is rejected below — reads are always to mapped addresses, never UB.
        let cam = unsafe { MmioCam::new(base as *mut u8, Cam::Ecam) };
        let candidate = PciRoot::new(cam);
        let bridge_valid = candidate
            .enumerate_bus(0)
            .next()
            .is_some_and(|(_, info)| info.vendor_id != 0 && info.vendor_id != 0xFFFF);
        if bridge_valid {
            let mut buf = alloc::string::String::new();
            let _ = core::fmt::write(&mut buf, format_args!("[E6.5] ECAM at {base:#x}\n"));
            serial(&buf);
            root = Some(candidate);
            break;
        }
    }
    let mut root = root?;

    let mut found = None;
    for (df, info) in root.enumerate_bus(0) {
        if virtio_device_type(&info) == Some(DeviceType::Network) {
            found = Some((df, info));
            break;
        }
    }
    let (df, info) = found?;
    let mut buf = alloc::string::String::new();
    let _ = core::fmt::write(
        &mut buf,
        format_args!("[E6.5] virtio-net at {df} ({info})\n"),
    );
    serial(&buf);

    // OVMF has already assigned BARs; make sure memory space + bus mastering
    // are on so the device can DMA the rings we hand it.
    root.set_command(df, Command::MEMORY_SPACE | Command::BUS_MASTER);

    let transport = PciTransport::new::<KernelHal, _>(&mut root, df).ok()?;
    VirtIONet::new(transport, NET_BUF_LEN).ok()
}

// ─── smoltcp glue ────────────────────────────────────────────────────────────

/// `smoltcp` device over the virtio NIC.
///
/// Receive copies the frame out and recycles the ring buffer immediately,
/// trading one memcpy for a borrow-free token (the bounded console exchange
/// is far from line-rate, so simplicity wins over zero-copy here).
struct VirtioSmolDev {
    net: Net,
}

struct VirtioRx(Vec<u8>);
struct VirtioTx<'a> {
    net: &'a mut Net,
}

impl RxToken for VirtioRx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for VirtioTx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut tx = self.net.new_tx_buffer(len);
        let result = f(tx.packet_mut());
        // A full TX ring on this bounded exchange means the device wedged;
        // drop the frame and let TCP retransmit rather than panicking.
        let _ = self.net.send(tx);
        result
    }
}

impl Device for VirtioSmolDev {
    type RxToken<'a> = VirtioRx;
    type TxToken<'a> = VirtioTx<'a>;

    fn receive(&mut self, _ts: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if !self.net.can_recv() {
            return None;
        }
        let rx = self.net.receive().ok()?;
        let frame = rx.packet().to_vec();
        let _ = self.net.recycle_rx_buffer(rx);
        Some((VirtioRx(frame), VirtioTx { net: &mut self.net }))
    }

    fn transmit(&mut self, _ts: Instant) -> Option<Self::TxToken<'_>> {
        self.net
            .can_send()
            .then_some(VirtioTx { net: &mut self.net })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(1);
        caps
    }
}

// ─── The phase ───────────────────────────────────────────────────────────────

/// Bring up virtio-net + smoltcp and run the console exchange with the host
/// listener. Self-skips (Ok, no markers) when no virtio-net device exists.
pub fn run_net_phase(serial: impl Fn(&str)) -> Result<(), &'static str> {
    let Some(net) = probe_virtio_net(&serial) else {
        serial("[E6.5] no virtio-net function on the ECAM — phase skipped\n");
        return Ok(());
    };

    let mac = net.mac_address();
    let mut buf = alloc::string::String::new();
    let _ = core::fmt::write(
        &mut buf,
        format_args!(
            "[E6.5] mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, smoltcp up on 10.0.2.15/24\n",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        ),
    );
    serial(&buf);

    let mut dev = VirtioSmolDev { net };
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = 0xA111A05; // deterministic: no entropy needed pre-TLS
    let mut iface = Interface::new(config, &mut dev, Instant::from_millis(0));
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(GUEST_IP), 24));
    });
    iface
        .routes_mut()
        .add_default_ipv4_route(HOST_ALIAS)
        .map_err(|_| "default route")?;

    let mut sockets = SocketSet::new(vec![]);
    let tcp_rx = tcp::SocketBuffer::new(vec![0; 4096]);
    let tcp_tx = tcp::SocketBuffer::new(vec![0; 4096]);
    let handle = sockets.add(tcp::Socket::new(tcp_rx, tcp_tx));

    {
        let socket = sockets.get_mut::<tcp::Socket>(handle);
        socket
            .connect(iface.context(), (HOST_ALIAS, CONSOLE_PORT), 49500)
            .map_err(|_| "tcp connect issue")?;
    }

    // Telemetry to egress once the handshake completes: a real OperatorEvent
    // in the exact COM1 Phase-0 framing, now over TCP.
    let event = OperatorEvent::State {
        lifecycle: alloc::string::String::from("Awake"),
        sleep_phase: None,
        agenda_depth: 0,
    };
    let mut tlm = alloc::string::String::from(TELEMETRY_PREFIX);
    tlm.push_str(&event.to_ndjson());
    tlm.push('\n');

    let mut sent = false;
    let mut line: Vec<u8> = Vec::new();
    let mut guidance_ok = false;
    // Coarse clock: smoltcp only needs monotonic millis for retransmit
    // timers, but a 1 ms tick per poll would run the fake clock ~50-100×
    // faster than wall time under QEMU — host-side latency of a couple of
    // real seconds would then look like minutes and fire premature TCP
    // retransmits/aborts. Tick 1 ms every 64 polls instead (fake time ≤
    // real time on every host measured) and budget ~30 fake-seconds; the
    // loop still returns the moment the exchange completes.
    for poll in 0..1_920_000u32 {
        let ts = Instant::from_millis(i64::from(poll / 64));
        iface.poll(ts, &mut dev, &mut sockets);
        let socket = sockets.get_mut::<tcp::Socket>(handle);

        if socket.may_send() && !sent {
            socket
                .send_slice(tlm.as_bytes())
                .map_err(|_| "tlm send failed")?;
            serial("[E6.5] ANIMA_TLM frame sent over TCP\n");
            sent = true;
        }

        if socket.can_recv() {
            let mut chunk = [0u8; 256];
            let n = socket.recv_slice(&mut chunk).map_err(|_| "recv failed")?;
            for &b in &chunk[..n] {
                if b == b'\n' {
                    if let Ok(text) = core::str::from_utf8(&line) {
                        if let Some(input) = parse_input_line(text) {
                            let mut msg = alloc::string::String::new();
                            let _ = core::fmt::write(
                                &mut msg,
                                format_args!(
                                    "[E6.5] guidance over TCP: \"{}\" (priority {:?})\n",
                                    input.text, input.priority
                                ),
                            );
                            serial(&msg);
                            guidance_ok = true;
                        }
                    }
                    line.clear();
                } else if line.len() < 512 {
                    line.push(b);
                }
            }
        }

        if sent && guidance_ok {
            socket.close();
        }
        if sent && guidance_ok && !socket.is_open() {
            serial("E6.5_NET_DONE\n");
            serial("E6.5_GUIDANCE_OK\n");
            return Ok(());
        }
    }

    Err(if !sent {
        "TCP handshake to the host console listener never completed"
    } else {
        "no ANIMA_IN guidance line arrived before the poll budget expired"
    })
}
