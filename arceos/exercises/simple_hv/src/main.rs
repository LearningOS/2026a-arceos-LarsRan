#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(asm_const)]
#![feature(riscv_ext_intrinsics)]

#[cfg(feature = "axstd")]
extern crate axstd as std;
extern crate alloc;
#[macro_use]
extern crate axlog;

mod task;
mod vcpu;
mod regs;
mod csrs;
mod sbi;
mod loader;

use vcpu::VmCpuRegisters;
use riscv::register::{scause, sstatus, stval};
use csrs::defs::hstatus;
use tock_registers::LocalRegisterCopy;
use csrs::{RiscvCsrTrait, CSR};
use vcpu::_run_guest;
use sbi::SbiMessage;
use loader::load_vm_image;
use axhal::mem::{phys_to_virt, PhysAddr, VirtAddr, PAGE_SIZE_4K};
use axhal::paging::MappingFlags;
use axmm::AddrSpace;
use crate::regs::GprIndex;
use crate::regs::GprIndex::{A0, A1};

const VM_ENTRY: usize = 0x8020_0000;

/// The hart id reported to the guest when it reads `mhartid`.
const GUEST_HART_ID: usize = 0x1234;

/// The word the guest expects to load from its fixed boot-info address (GPA 0x40).
const BOOT_MAGIC: u64 = 0x6688;

/// `mhartid` CSR number, decoded from the `csrr` instruction that traps.
const CSR_MHARTID: usize = 0xF14;

/// Bit 1 of `sstatus`: supervisor interrupt enable.
const SSTATUS_SIE: usize = 1 << 1;

/// Bit 5 of `sie`: supervisor timer interrupt enable.
const SIE_STIE: usize = 1 << 5;

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // A new address space for vm.
    let mut uspace = axmm::new_user_aspace().unwrap();

    // Load vm binary file into address space.
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // Setup context to prepare to enter guest mode.
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // Setup pagetable for 2nd address mapping.
    let ept_root = uspace.page_table_root();
    prepare_vm_pgtable(ept_root);

    // Kick off vm and wait for it to exit.
    while !run_guest(&mut ctx, &mut uspace) {}

    panic!("Hypervisor ok!");
}

fn prepare_vm_pgtable(ept_root: PhysAddr) {
    let hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
    }
    flush_g_stage_tlb();
}

/// Flushes the G-stage TLB after the guest page table is modified.
fn flush_g_stage_tlb() {
    unsafe { core::arch::riscv64::hfence_gvma_all() };
}

/// Reads the 32-bit instruction at the guest physical address `gpa`.
///
/// The guest runs with a bare VS-stage page table here, so the faulting `sepc`
/// is a guest physical address and can be looked up in the EPT directly.
fn read_insn(uspace: &AddrSpace, gpa: usize) -> u32 {
    let page = gpa & !(PAGE_SIZE_4K - 1);
    let offset = gpa & (PAGE_SIZE_4K - 1);
    let (paddr, _, _) = uspace
        .page_table()
        .query(VirtAddr::from(page))
        .expect("cannot translate guest instruction address");
    unsafe { ((phys_to_virt(paddr).as_usize() + offset) as *const u32).read_volatile() }
}

/// Adds a zeroed, writable page for the guest physical address `gpa`.
fn map_guest_page(uspace: &mut AddrSpace, gpa: usize) {
    let page = gpa & !(PAGE_SIZE_4K - 1);
    uspace
        .map_alloc(
            VirtAddr::from(page),
            PAGE_SIZE_4K,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            true,
        )
        .unwrap_or_else(|e| panic!("cannot map guest page {:#x}: {:?}", page, e));
    flush_g_stage_tlb();
}

fn run_guest(ctx: &mut VmCpuRegisters, uspace: &mut AddrSpace) -> bool {
    unsafe {
        _run_guest(ctx);
    }

    vmexit_handler(ctx, uspace)
}

#[allow(unreachable_code)]
fn vmexit_handler(ctx: &mut VmCpuRegisters, uspace: &mut AddrSpace) -> bool {
    use scause::{Exception, Trap};

    let scause = scause::read();
    match scause.cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            ax_println!("VmExit Reason: VSuperEcall: {:?}", sbi_msg);
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        let a0 = ctx.guest_regs.gprs.reg(A0);
                        let a1 = ctx.guest_regs.gprs.reg(A1);
                        ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
                        assert_eq!(a0, BOOT_MAGIC as usize);
                        assert_eq!(a1, GUEST_HART_ID);
                        ax_println!("Shutdown vm normally!");
                        return true;
                    },
                    _ => todo!(),
                }
            } else {
                panic!("bad sbi message! ");
            }
        },
        Trap::Exception(Exception::IllegalInstruction) => {
            // `mhartid` is an M-mode CSR, so a VS-mode guest reading it always traps
            // here. Emulate the read with the hart id we present to this VM.
            let insn = read_insn(uspace, ctx.guest_regs.sepc);
            let rd = (insn >> 7) & 0x1f;
            let funct3 = (insn >> 12) & 0x7;
            let csr = (insn >> 20) as usize;
            if funct3 == 0b010 && csr == CSR_MHARTID {
                let rd = GprIndex::from_raw(rd).expect("bad rd in csrr");
                ctx.guest_regs.gprs.set_reg(rd, GUEST_HART_ID);
                ctx.guest_regs.sepc += 4;
            } else {
                panic!(
                    "Bad instruction: {:#x} sepc: {:#x}",
                    insn,
                    ctx.guest_regs.sepc
                );
            }
        },
        Trap::Exception(Exception::LoadGuestPageFault) => {
            // The guest reads its boot-info word from a fixed low address that has
            // no mapping yet: back the page on demand and fill in the value, then
            // re-execute the faulting load.
            let gpa = stval::read();
            map_guest_page(uspace, gpa);

            let page = gpa & !(PAGE_SIZE_4K - 1);
            let offset = gpa & (PAGE_SIZE_4K - 1);
            let (paddr, _, _) = uspace
                .page_table()
                .query(VirtAddr::from(page))
                .expect("cannot translate guest data address");
            unsafe {
                ((phys_to_virt(paddr).as_usize() + offset) as *mut u64)
                    .write_volatile(BOOT_MAGIC)
            };
        },
        _ => {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                stval::read()
            );
        }
    }
    false
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // Keep the host from taking interrupts while it drives the guest.
    //
    // `_restore_csrs` writes the host `sstatus` back (which has SIE set) *before* it
    // switches `stvec` back to the host vector, and `_run_guest` installs `stvec`
    // before it stashes the GuestInfo pointer in `sscratch`. A timer tick landing in
    // either window is delivered to `_guest_exit`, whose `csrrw a0, sscratch, a0`
    // then loads the host's `sscratch` (0) instead of the GuestInfo pointer, so the
    // following `sd ra, 296(a0)` faults on address 0x128 and traps back into
    // `_guest_exit` forever.
    //
    // Disabling SIE alone is not enough: while V = 1 the HS-level timer interrupt is
    // delivered regardless, and it lands on `_guest_exit` too. Masking the timer
    // source removes it completely, and SIE is cleared as well so the windows where
    // the host runs with V = 0 stay quiet.
    //
    // Clearing them before the `sstatus::read()` below also keeps the context handed
    // to the guest consistent with the live CSRs.
    unsafe {
        core::arch::asm!(
            "csrc sstatus, {sie}",
            "csrc sie, {stie}",
            sie = in(reg) SSTATUS_SIE,
            stie = in(reg) SIE_STIE,
            options(nomem, nostack),
        );
    }

    // Set hstatus
    let mut hstatus = LocalRegisterCopy::<usize, hstatus::Register>::new(
        riscv::register::hstatus::read().bits(),
    );
    // Set Guest bit in order to return to guest mode.
    hstatus.modify(hstatus::spv::Guest);
    // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // Set sstatus in guest mode.
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    ctx.guest_regs.sstatus = sstatus.bits();
    // Return to entry to start vm.
    ctx.guest_regs.sepc = VM_ENTRY;
}
