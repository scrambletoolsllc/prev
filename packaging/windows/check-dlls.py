"""Makes sure prev's Windows files run on a Windows with nothing else
installed: every DLL that a program or DLL in DIR loads must be in DIR,
or be part of Windows itself.

  check-dlls.py DIR [--bundle-crt x64|arm64] [--system-list FILE]

--bundle-crt first copies in the Visual C++ runtime DLLs that the files
load (MSVCP140.dll, VCRUNTIME140.dll and their kin), from the installed
Visual Studio's redistributable files, which Microsoft lets apps ship
beside them; without them prev does not start on a Windows that lacks the
Visual C++ Redistributable. --system-list writes the Windows DLLs the
files load, one a line, for the install test to look for on a clean
Windows. Needs pefile (pip install pefile).
"""

import argparse
import glob
import os
import shutil
import subprocess
import sys

import pefile

# DLLs that every Windows 10 and 11 has, which prev's files may load. A
# DLL missing here and from DIR fails the check: add it only once it is
# known to come with Windows, never for one of a redistributable.
SYSTEM = {
    "advapi32.dll", "bcrypt.dll", "bcryptprimitives.dll", "cfgmgr32.dll", "combase.dll",
    "comctl32.dll", "comdlg32.dll", "crypt32.dll", "d2d1.dll", "d3d11.dll", "d3d12.dll",
    "d3dcompiler_47.dll", "dbghelp.dll", "dwmapi.dll", "dwrite.dll", "dxgi.dll", "gdi32.dll",
    "gdiplus.dll", "hid.dll", "imm32.dll", "iphlpapi.dll", "kernel32.dll", "kernelbase.dll",
    "msimg32.dll", "mswsock.dll", "ncrypt.dll", "netapi32.dll", "ntdll.dll", "ole32.dll",
    "oleacc.dll", "oleaut32.dll", "opengl32.dll", "powrprof.dll", "propsys.dll", "psapi.dll",
    "rpcrt4.dll", "secur32.dll", "setupapi.dll", "shcore.dll", "shell32.dll", "shlwapi.dll",
    "ucrtbase.dll", "user32.dll", "userenv.dll", "uxtheme.dll", "version.dll", "winhttp.dll",
    "winmm.dll", "winspool.drv", "ws2_32.dll", "wtsapi32.dll",
}


def system(name):
    # API sets resolve to Windows' own DLLs, the Universal C runtime among
    # them.
    return name in SYSTEM or name.startswith(("api-ms-win-", "ext-ms-win-"))


def imports(path):
    pe = pefile.PE(path, fast_load=True)
    pe.parse_data_directories(directories=[
        pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"],
        pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT"],
    ])
    names = set()
    for entry in getattr(pe, "DIRECTORY_ENTRY_IMPORT", []) + getattr(pe, "DIRECTORY_ENTRY_DELAY_IMPORT", []):
        names.add(entry.dll.decode().lower())
    return names


def crt_folder(arch):
    """The newest Visual C++ runtime folder of the installed Visual Studio
    for `arch`, such as ...\\VC\\Redist\\MSVC\\14.44.35112\\x64\\Microsoft.VC143.CRT."""
    vswhere = os.path.join(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"),
                           "Microsoft Visual Studio", "Installer", "vswhere.exe")
    roots = subprocess.run([vswhere, "-all", "-products", "*", "-property", "installationPath"],
                           capture_output=True, text=True, check=True).stdout.splitlines()
    roots = [root.strip() for root in roots if root.strip()]
    found = []
    for root in roots:
        found += glob.glob(os.path.join(root, "VC", "Redist", "MSVC", "*", arch, "Microsoft.VC*.CRT"))
    if not found:
        sys.exit(f"No Visual C++ runtime for {arch} in Visual Studio at {roots}")

    def version(path):
        parts = path.split(os.sep)
        number = parts[parts.index("MSVC") + 1]
        return [int(p) if p.isdigit() else 0 for p in number.split(".")]

    return max(found, key=version)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dir")
    parser.add_argument("--bundle-crt", choices=["x64", "arm64"])
    parser.add_argument("--system-list")
    args = parser.parse_args()

    crt = crt_folder(args.bundle_crt) if args.bundle_crt else None
    if crt:
        print(f"Visual C++ runtime from {crt}")
        available = {name.lower(): os.path.join(crt, name) for name in os.listdir(crt)}

    # Every file's imports, and those of the runtime DLLs copied in, until
    # nothing new comes.
    shipped = {name.lower(): name for name in os.listdir(args.dir)}
    pending = [n for n in shipped if n.endswith((".exe", ".dll"))]
    seen, used, missing = set(), set(), {}
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for dll in sorted(imports(os.path.join(args.dir, shipped[name]))):
            if dll in shipped:
                pending.append(dll)
            elif system(dll):
                used.add(dll)
            elif crt and dll in available:
                shutil.copy2(available[dll], args.dir)
                copied = os.path.basename(available[dll])
                shipped[dll] = copied
                print(f"copied {copied}, which {shipped[name]} loads")
                pending.append(dll)
            else:
                missing.setdefault(dll, []).append(shipped[name])

    for name in sorted(seen):
        print(f"{shipped[name]}: {', '.join(sorted(imports(os.path.join(args.dir, shipped[name]))))}")
    if args.system_list:
        with open(args.system_list, "w") as f:
            f.write("".join(f"{dll}\n" for dll in sorted(used) if not dll.startswith(("api-ms-", "ext-ms-"))))
    if missing:
        for dll, users in sorted(missing.items()):
            print(f"MISSING {dll}, which {', '.join(users)} loads: neither shipped nor part of Windows",
                  file=sys.stderr)
        sys.exit(1)
    print("Every DLL these files load is shipped beside them or comes with Windows.")


if __name__ == "__main__":
    main()
