"""Local tool: a sampling profiler for a running victory process (same user, no admin).

python -I sampler.py <image name> <pdb dir> [seconds] [interval ms] [min thread ms]

Suspends each thread in turn, walks its stack with dbghelp and resumes it. Prints, for the
threads that were busiest in list calls, the functions of the main module by inclusive samples,
and where the innermost frames are (module or function).
"""
import collections
import ctypes
import ctypes.wintypes as wt
import subprocess
import sys
import time

image = sys.argv[1]
pdb_dir = sys.argv[2]
seconds = float(sys.argv[3]) if len(sys.argv) > 3 else 6.0
interval = float(sys.argv[4]) / 1000 if len(sys.argv) > 4 else 0.004

k32 = ctypes.WinDLL('kernel32', use_last_error=True)
dbg = ctypes.WinDLL('dbghelp', use_last_error=True)

out = subprocess.run(['tasklist', '/FI', f'IMAGENAME eq {image}', '/FO', 'CSV', '/NH'], capture_output=True, text=True).stdout
pid = int(out.split(',')[1].strip('"'))

PROCESS_ALL = 0x0400 | 0x0010 | 0x0008 | 0x0040  # query, vm read, vm operation, dup handle
k32.OpenProcess.restype = wt.HANDLE
proc = k32.OpenProcess(0x1F0FFF, False, pid)
assert proc, ctypes.get_last_error()


class THREADENTRY32(ctypes.Structure):
    _fields_ = [('dwSize', wt.DWORD), ('cntUsage', wt.DWORD), ('th32ThreadID', wt.DWORD), ('th32OwnerProcessID', wt.DWORD),
                ('tpBasePri', wt.LONG), ('tpDeltaPri', wt.LONG), ('dwFlags', wt.DWORD)]


k32.CreateToolhelp32Snapshot.restype = wt.HANDLE
snap = k32.CreateToolhelp32Snapshot(4, 0)
te = THREADENTRY32()
te.dwSize = ctypes.sizeof(te)
tids = []
ok = k32.Thread32First(snap, ctypes.byref(te))
while ok:
    if te.th32OwnerProcessID == pid:
        tids.append(te.th32ThreadID)
    ok = k32.Thread32Next(snap, ctypes.byref(te))
k32.CloseHandle(snap)

k32.OpenThread.restype = wt.HANDLE
threads = {}
for t in tids:
    h = k32.OpenThread(0x0002 | 0x0008 | 0x0040, False, t)  # suspend/resume, get context, query
    if h:
        threads[t] = h


class M128A(ctypes.Structure):
    _fields_ = [('Low', ctypes.c_ulonglong), ('High', ctypes.c_longlong)]


class CONTEXT(ctypes.Structure):
    _pack_ = 16
    _fields_ = [('P1Home', ctypes.c_ulonglong), ('P2Home', ctypes.c_ulonglong), ('P3Home', ctypes.c_ulonglong),
                ('P4Home', ctypes.c_ulonglong), ('P5Home', ctypes.c_ulonglong), ('P6Home', ctypes.c_ulonglong),
                ('ContextFlags', wt.DWORD), ('MxCsr', wt.DWORD),
                ('SegCs', wt.WORD), ('SegDs', wt.WORD), ('SegEs', wt.WORD), ('SegFs', wt.WORD), ('SegGs', wt.WORD), ('SegSs', wt.WORD),
                ('EFlags', wt.DWORD),
                ('Dr0', ctypes.c_ulonglong), ('Dr1', ctypes.c_ulonglong), ('Dr2', ctypes.c_ulonglong), ('Dr3', ctypes.c_ulonglong),
                ('Dr6', ctypes.c_ulonglong), ('Dr7', ctypes.c_ulonglong),
                ('Rax', ctypes.c_ulonglong), ('Rcx', ctypes.c_ulonglong), ('Rdx', ctypes.c_ulonglong), ('Rbx', ctypes.c_ulonglong),
                ('Rsp', ctypes.c_ulonglong), ('Rbp', ctypes.c_ulonglong), ('Rsi', ctypes.c_ulonglong), ('Rdi', ctypes.c_ulonglong),
                ('R8', ctypes.c_ulonglong), ('R9', ctypes.c_ulonglong), ('R10', ctypes.c_ulonglong), ('R11', ctypes.c_ulonglong),
                ('R12', ctypes.c_ulonglong), ('R13', ctypes.c_ulonglong), ('R14', ctypes.c_ulonglong), ('R15', ctypes.c_ulonglong),
                ('Rip', ctypes.c_ulonglong),
                ('FltSave', ctypes.c_byte * 512),
                ('VectorRegister', M128A * 26), ('VectorControl', ctypes.c_ulonglong),
                ('DebugControl', ctypes.c_ulonglong), ('LastBranchToRip', ctypes.c_ulonglong), ('LastBranchFromRip', ctypes.c_ulonglong),
                ('LastExceptionToRip', ctypes.c_ulonglong), ('LastExceptionFromRip', ctypes.c_ulonglong)]


assert ctypes.sizeof(CONTEXT) == 1232, ctypes.sizeof(CONTEXT)


class ADDRESS64(ctypes.Structure):
    _fields_ = [('Offset', ctypes.c_ulonglong), ('Segment', wt.WORD), ('Mode', wt.DWORD)]


class KDHELP64(ctypes.Structure):
    _fields_ = [('pad', ctypes.c_ulonglong * 14)]


class STACKFRAME64(ctypes.Structure):
    _fields_ = [('AddrPC', ADDRESS64), ('AddrReturn', ADDRESS64), ('AddrFrame', ADDRESS64), ('AddrStack', ADDRESS64),
                ('AddrBStore', ADDRESS64), ('FuncTableEntry', ctypes.c_void_p), ('Params', ctypes.c_ulonglong * 4),
                ('Far', wt.BOOL), ('Virtual', wt.BOOL), ('Reserved', ctypes.c_ulonglong * 3), ('KdHelp', KDHELP64)]


class SYMBOL_INFOW(ctypes.Structure):
    _fields_ = [('SizeOfStruct', wt.ULONG), ('TypeIndex', wt.ULONG), ('Reserved', ctypes.c_ulonglong * 2),
                ('Index', wt.ULONG), ('Size', wt.ULONG), ('ModBase', ctypes.c_ulonglong), ('Flags', wt.ULONG),
                ('Value', ctypes.c_ulonglong), ('Address', ctypes.c_ulonglong), ('Register', wt.ULONG),
                ('Scope', wt.ULONG), ('Tag', wt.ULONG), ('NameLen', wt.ULONG), ('MaxNameLen', wt.ULONG),
                ('Name', wt.WCHAR * 1024)]


class IMAGEHLP_MODULEW64(ctypes.Structure):
    _fields_ = [('SizeOfStruct', wt.DWORD), ('BaseOfImage', ctypes.c_ulonglong), ('ImageSize', wt.DWORD),
                ('TimeDateStamp', wt.DWORD), ('CheckSum', wt.DWORD), ('NumSyms', wt.DWORD), ('SymType', wt.DWORD),
                ('ModuleName', wt.WCHAR * 32), ('ImageName', wt.WCHAR * 256), ('LoadedImageName', wt.WCHAR * 256),
                ('rest', ctypes.c_byte * 4096)]


dbg.SymSetOptions(0x2 | 0x4 | 0x10)  # undname, deferred loads, load lines off
dbg.SymInitializeW.argtypes = [wt.HANDLE, wt.LPCWSTR, wt.BOOL]
assert dbg.SymInitializeW(proc, pdb_dir, True), ctypes.get_last_error()
dbg.SymFromAddrW.argtypes = [wt.HANDLE, ctypes.c_ulonglong, ctypes.POINTER(ctypes.c_ulonglong), ctypes.POINTER(SYMBOL_INFOW)]
dbg.SymGetModuleInfoW64.argtypes = [wt.HANDLE, ctypes.c_ulonglong, ctypes.POINTER(IMAGEHLP_MODULEW64)]
dbg.SymFunctionTableAccess64.restype = ctypes.c_void_p
dbg.SymFunctionTableAccess64.argtypes = [wt.HANDLE, ctypes.c_ulonglong]
dbg.SymGetModuleBase64.restype = ctypes.c_ulonglong
dbg.SymGetModuleBase64.argtypes = [wt.HANDLE, ctypes.c_ulonglong]
dbg.StackWalk64.argtypes = [wt.DWORD, wt.HANDLE, wt.HANDLE, ctypes.POINTER(STACKFRAME64), ctypes.c_void_p,
                            ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
k32.GetThreadContext.argtypes = [wt.HANDLE, ctypes.c_void_p]
k32.SuspendThread.argtypes = [wt.HANDLE]
k32.ResumeThread.argtypes = [wt.HANDLE]

ctx_buf = ctypes.create_string_buffer(ctypes.sizeof(CONTEXT) + 16)
ctx_at = (ctypes.addressof(ctx_buf) + 15) & ~15
ctx = CONTEXT.from_address(ctx_at)

stacks = collections.defaultdict(list)   # tid -> list of tuples of addresses (innermost first)
deadline = time.perf_counter() + seconds
rounds = 0
while time.perf_counter() < deadline:
    for tid, h in threads.items():
        if k32.SuspendThread(h) == 0xFFFFFFFF:
            continue
        ctx.ContextFlags = 0x100000 | 0x1 | 0x2  # amd64 control + integer
        frames = []
        if k32.GetThreadContext(h, ctx_at):
            sf = STACKFRAME64()
            sf.AddrPC.Offset, sf.AddrPC.Mode = ctx.Rip, 3
            sf.AddrFrame.Offset, sf.AddrFrame.Mode = ctx.Rbp, 3
            sf.AddrStack.Offset, sf.AddrStack.Mode = ctx.Rsp, 3
            for _ in range(48):
                if not dbg.StackWalk64(0x8664, proc, h, ctypes.byref(sf), ctx_at, None,
                                       dbg.SymFunctionTableAccess64, dbg.SymGetModuleBase64, None):
                    break
                if sf.AddrPC.Offset == 0:
                    break
                frames.append(sf.AddrPC.Offset)
        k32.ResumeThread(h)
        if frames:
            stacks[tid].append(tuple(frames))
    rounds += 1
    time.sleep(interval)

names = {}


def name_of(addr):
    if addr in names:
        return names[addr]
    info = SYMBOL_INFOW()
    info.SizeOfStruct = 88
    info.MaxNameLen = 1024
    disp = ctypes.c_ulonglong(0)
    mod = IMAGEHLP_MODULEW64()
    mod.SizeOfStruct = ctypes.sizeof(IMAGEHLP_MODULEW64)
    module = mod.ModuleName if dbg.SymGetModuleInfoW64(proc, addr, ctypes.byref(mod)) else '?'
    fn = info.Name if dbg.SymFromAddrW(proc, addr, ctypes.byref(disp), ctypes.byref(info)) else None
    names[addr] = (module, fn)
    return names[addr]


main = image.rsplit('.', 1)[0].lower()
# FILTER: the frames a sample must have to count (argv[5] if it is not a number).
FILTER = sys.argv[5] if len(sys.argv) > 5 and not sys.argv[5].replace('.', '').isdigit() else 'victory_gpu::list::'
print(f'pid {pid}, {len(threads)} threads, {rounds} rounds')
# Threads that spend their samples in list calls: those with `victory_gpu::list::` on the stack.
rank = []
for tid, ss in stacks.items():
    inlist = 0
    for st in ss:
        if any(FILTER in (name_of(a)[1] or '') for a in st):
            inlist += 1
    rank.append((inlist, tid, len(ss)))
rank.sort(reverse=True)
chosen = [t for n, t, _ in rank[:6] if n > 0]
print('threads in list calls:', [(t, n, total) for n, t, total in rank[:6]])
incl = collections.Counter()
leaf = collections.Counter()
total = 0
for tid in chosen:
    for st in stacks[tid]:
        syms = [name_of(a) for a in st]
        if not any(FILTER in (fn or '') for _, fn in syms):
            continue
        total += 1
        seen = set()
        for module, fn in syms:
            if fn and module.lower().startswith(main[:8]) and fn not in seen:
                seen.add(fn)
                incl[fn] += 1
        module, fn = syms[0]
        leaf[(module, fn if module.lower().startswith(main[:8]) else (fn or '?'))] += 1
# CHAINS: for the functions named in argv[6:], the victory frames around them.
want = sys.argv[6:]
chains = collections.Counter()
for tid in chosen:
    for st in stacks[tid]:
        syms = [name_of(a) for a in st]
        fns = [fn for module, fn in syms if fn and module.lower().startswith(main[:8])]
        for w in want:
            if (w.startswith('@') and w[1:] in (syms[0][1] or '')) or (not w.startswith('@') and any(w in f for f in fns)):
                short = [f.split('<')[0].replace('victory_gpu::', '') for f in fns if 'list::with_state' not in f][:9]
                chains[(w, ' < '.join(short))] += 1
for (w, chain), n in chains.most_common(14):
    print(f'  chain[{w}] x{n}: {chain[:420]}')
print(f'{total} samples inside list calls on those threads')
print('== inclusive, functions of the main module:')
for fn, n in incl.most_common(60):
    print(f'  {100 * n / max(total, 1):5.1f}%  {fn[:150]}')
print('== innermost frame:')
for (module, fn), n in leaf.most_common(40):
    print(f'  {100 * n / max(total, 1):5.1f}%  {module}!{(fn or "?")[:130]}')
