//! E6.5 follow-on — ACPI MCFG discovery of the PCI ECAM base.
//!
//! `net.rs` brings up virtio-net by walking the PCI ECAM (MMCONFIG) window. The
//! base of that window is board-specific: OVMF on QEMU q35 programs
//! `0xE000_0000`, other firmwares (SeaBIOS-era q35, some Cloud Hypervisor
//! builds) use different addresses. The original probe guessed from a small
//! hard-coded candidate list (`net::ECAM_CANDIDATES`); the durable fix — tracked
//! in `docs/22` §1 — is to read the real base out of the ACPI **MCFG** table,
//! which every UEFI/ACPI platform that exposes PCIe ECAM is required to publish.
//!
//! This module obtains the ACPI **RSDP** from the UEFI configuration table
//! (boot services are still live — the kernel never calls `exit_boot_services`),
//! walks the XSDT (or the ACPI 1.0 RSDT), finds the MCFG table, and returns the
//! ECAM base address(es) it declares. `net.rs` tries these first and falls back
//! to the hard-coded candidates, so a board without MCFG behaves exactly as
//! before. Every returned base is still validated by the host-bridge vendor-id
//! check in `net::probe_virtio_net`, so a malformed table can only degrade to
//! "no device", never to undefined behaviour.
//!
//! # Layering
//!
//! The byte-level parsers ([`parse_rsdp`], [`parse_mcfg_bases`]) are **pure**
//! functions over `&[u8]`: no raw pointers, no platform calls. They are
//! validated against synthetic ACPI tables on the host (the UEFI target cannot
//! run `cargo test`). The single `unsafe` surface — dereferencing the
//! firmware-supplied physical pointers — is confined to [`mcfg_ecam_bases`] and
//! its helpers and is recorded in `crates/corpus/unsafe_audit.md` §3.

use alloc::vec::Vec;

use uefi::table::cfg::ConfigTableEntry;

/// `"RSD PTR "` — the 8-byte RSDP signature.
const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
/// Length of the common ACPI System Description Table header.
const SDT_HEADER_LEN: usize = 36;
/// `"MCFG"` — the PCI Express memory-mapped configuration table signature.
const MCFG_SIG: &[u8; 4] = b"MCFG";
/// Byte offset of the first allocation structure inside an MCFG table (36-byte
/// header + 8 reserved bytes).
const MCFG_ALLOC_OFFSET: usize = 44;
/// Size of one MCFG configuration-space allocation structure.
const MCFG_ALLOC_LEN: usize = 16;

/// Upper bound on an SDT length we are willing to map, so a corrupt length field
/// cannot make us read an absurd span of memory. The largest ACPI tables in
/// practice are a few KiB; 64 KiB is generous.
const MAX_TABLE_LEN: usize = 64 * 1024;
/// Upper bound on root-table (XSDT/RSDT) entries we will follow, bounding the
/// scan against a corrupt length field.
const MAX_ROOT_ENTRIES: usize = 256;

/// The root system table the RSDP points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootTable {
    /// Physical address of the XSDT (64-bit entries) or RSDT (32-bit entries).
    pub addr: u64,
    /// `true` when `addr` is an XSDT (entries are `u64`), `false` for an RSDT.
    pub is_xsdt: bool,
}

// ─── pure parsers (host-validated; no raw pointers) ─────────────────────────

/// Validate an RSDP and return the root table it points at, preferring the
/// 64-bit XSDT when the structure is ACPI 2.0+. Returns `None` if the signature
/// is wrong, the buffer is too short, or both root pointers are null.
pub fn parse_rsdp(rsdp: &[u8]) -> Option<RootTable> {
    if rsdp.len() < 20 || &rsdp[0..8] != RSDP_SIG {
        return None;
    }
    let revision = rsdp[15];
    // ACPI 2.0+ RSDPs are 36 bytes and carry a 64-bit XSDT pointer at offset 24.
    if revision >= 2 && rsdp.len() >= 36 {
        let xsdt = u64::from_le_bytes(rsdp[24..32].try_into().ok()?);
        if xsdt != 0 {
            return Some(RootTable {
                addr: xsdt,
                is_xsdt: true,
            });
        }
    }
    // Fall back to the ACPI 1.0 32-bit RSDT pointer at offset 16.
    let rsdt = u32::from_le_bytes(rsdp[16..20].try_into().ok()?);
    if rsdt == 0 {
        return None;
    }
    Some(RootTable {
        addr: u64::from(rsdt),
        is_xsdt: false,
    })
}

/// The 4-byte ACPI signature of an SDT given its header (`>= 4` bytes).
fn sdt_signature(header: &[u8]) -> Option<[u8; 4]> {
    header.get(0..4)?.try_into().ok()
}

/// The total length field of an SDT given its header (`>= 8` bytes).
fn sdt_length(header: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(header.get(4..8)?.try_into().ok()?))
}

/// Parse the ECAM base addresses out of a complete MCFG table.
///
/// Zero bases and partial/over-declared trailing entries are skipped; an
/// over-long declared length is clamped to the buffer, so this never reads out
/// of bounds regardless of the table contents.
pub fn parse_mcfg_bases(mcfg: &[u8]) -> Vec<u64> {
    let mut bases = Vec::new();
    if mcfg.len() < MCFG_ALLOC_OFFSET || sdt_signature(mcfg) != Some(*MCFG_SIG) {
        return bases;
    }
    let declared = sdt_length(mcfg).unwrap_or(0) as usize;
    // Trust the smaller of the declared length and the buffer we actually hold.
    let end = declared.min(mcfg.len());
    let mut off = MCFG_ALLOC_OFFSET;
    while off + MCFG_ALLOC_LEN <= end {
        let base = u64::from_le_bytes(mcfg[off..off + 8].try_into().unwrap());
        if base != 0 {
            bases.push(base);
        }
        off += MCFG_ALLOC_LEN;
    }
    bases
}

// ─── unsafe glue: firmware-supplied physical pointers ───────────────────────

/// Physical address of the ACPI RSDP, read from the UEFI configuration table.
///
/// Prefers the ACPI 2.0 entry (`ACPI2_GUID`) and falls back to the ACPI 1.0
/// entry (`ACPI_GUID`). `None` when neither is present.
fn rsdp_phys_addr() -> Option<u64> {
    uefi::system::with_config_table(|entries: &[ConfigTableEntry]| {
        let mut acpi1: Option<u64> = None;
        for e in entries {
            if e.guid == ConfigTableEntry::ACPI2_GUID {
                return Some(e.address as usize as u64);
            }
            if e.guid == ConfigTableEntry::ACPI_GUID {
                acpi1 = Some(e.address as usize as u64);
            }
        }
        acpi1
    })
}

/// Borrow `len` bytes of firmware memory at identity-mapped physical address
/// `phys` as a slice.
///
/// # Safety
///
/// `phys` must be a non-null physical address of at least `len` readable bytes
/// of firmware/ACPI memory that stays valid for the borrow. Under UEFI boot
/// services every such region is identity-mapped and live, which the callers
/// here guarantee: `phys` always comes from the UEFI configuration table (the
/// RSDP) or from a length-bounded pointer inside an already-validated ACPI
/// table.
unsafe fn map_bytes<'a>(phys: u64, len: usize) -> Option<&'a [u8]> {
    if phys == 0 || len == 0 {
        return None;
    }
    // SAFETY: see the function contract; the address is identity-mapped firmware
    // memory and `len` is bounded by the caller.
    Some(core::slice::from_raw_parts(phys as *const u8, len))
}

/// Read an SDT's full bytes given the physical address of its header.
///
/// Reads the 36-byte header first to learn the declared length, clamps it to
/// [`MAX_TABLE_LEN`], then re-borrows the full span. `None` on a null pointer or
/// an implausible length.
///
/// # Safety
///
/// `phys` must point at a readable ACPI SDT header (see [`map_bytes`]).
unsafe fn read_sdt<'a>(phys: u64) -> Option<&'a [u8]> {
    // SAFETY: caller guarantees `phys` is a readable SDT header; we read exactly
    // the fixed-size header before trusting any length field.
    let header = map_bytes(phys, SDT_HEADER_LEN)?;
    let len = sdt_length(header)? as usize;
    if !(SDT_HEADER_LEN..=MAX_TABLE_LEN).contains(&len) {
        return None;
    }
    // SAFETY: `len` is now bounded to a sane SDT size; the table is contiguous
    // identity-mapped firmware memory starting at `phys`.
    map_bytes(phys, len)
}

/// Discover PCI ECAM base address(es) by reading the ACPI MCFG table.
///
/// Returns an empty vector when there is no ACPI RSDP, no MCFG table, or the
/// MCFG declares no usable allocation. Callers must still validate each base
/// against real hardware (the host-bridge vendor-id check in `net.rs`).
pub fn mcfg_ecam_bases(serial: &impl Fn(&str)) -> Vec<u64> {
    let Some(rsdp_phys) = rsdp_phys_addr() else {
        serial("[E6.5] no ACPI RSDP in the UEFI config table — MCFG scan skipped\n");
        return Vec::new();
    };

    // SAFETY: `rsdp_phys` comes straight from the UEFI configuration table,
    // which the firmware populated with a valid, identity-mapped RSDP pointer.
    let Some(rsdp) = (unsafe { map_bytes(rsdp_phys, 36) }) else {
        return Vec::new();
    };
    let Some(root) = parse_rsdp(rsdp) else {
        serial("[E6.5] ACPI RSDP present but unparseable — MCFG scan skipped\n");
        return Vec::new();
    };

    // SAFETY: `root.addr` is the XSDT/RSDT pointer the validated RSDP published;
    // `read_sdt` bounds the length before mapping the full table.
    let Some(root_sdt) = (unsafe { read_sdt(root.addr) }) else {
        return Vec::new();
    };

    let entry_size = if root.is_xsdt { 8 } else { 4 };
    let entries_bytes = root_sdt.len().saturating_sub(SDT_HEADER_LEN);
    let count = (entries_bytes / entry_size).min(MAX_ROOT_ENTRIES);

    for i in 0..count {
        let off = SDT_HEADER_LEN + i * entry_size;
        let table_phys = if root.is_xsdt {
            u64::from_le_bytes(root_sdt[off..off + 8].try_into().unwrap())
        } else {
            u64::from(u32::from_le_bytes(
                root_sdt[off..off + 4].try_into().unwrap(),
            ))
        };

        // SAFETY: `table_phys` is an SDT pointer from the root table; `read_sdt`
        // validates and bounds it before mapping.
        let Some(table) = (unsafe { read_sdt(table_phys) }) else {
            continue;
        };
        if sdt_signature(table) == Some(*MCFG_SIG) {
            let bases = parse_mcfg_bases(table);
            let mut buf = alloc::string::String::new();
            let _ = core::fmt::write(
                &mut buf,
                format_args!(
                    "[E6.5] ACPI MCFG: {} ECAM base(s) discovered\n",
                    bases.len()
                ),
            );
            serial(&buf);
            return bases;
        }
    }

    serial("[E6.5] ACPI tables present but no MCFG — using ECAM candidate list\n");
    Vec::new()
}
