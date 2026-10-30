# limelisten 构建脚本
#
# 为什么需要它：本机装的是 Visual Studio 18 Community，而 rustc 自带的 MSVC 探测
# （vswhere/注册表）认不出它，导致 `link.exe` 找不到、`cc` 也编不了 bundled SQLite。
# 这里先导入 vcvars64 的环境再调 cargo，等价于"开发者命令提示符里跑 cargo"。
#
# 用法：  .\build.ps1              # cargo build
#         .\build.ps1 run          # cargo run
#         .\build.ps1 build --release   # 发布版（自带静态 CRT 与 x86-64-v3，见下）
#
# 静态 CRT（+crt-static）与微架构目标（target-cpu=x86-64-v3）：
# release 产物把 C/C++ 运行库静态链进 exe，不再依赖 vcruntime140.dll / msvcp140.dll；
# 同时指定 target-cpu=x86-64-v3，启用 AVX2/FMA/BMI2/LZCNT 指令集加速音频重采样、
# 变速拉伸与 DSP 计算。debug 构建不加，保持增量编译速度与通用调试兼容性。
$ErrorActionPreference = 'Stop'

$CargoArgs = @($args)
if (-not $CargoArgs -or $CargoArgs.Count -eq 0) { $CargoArgs = @('build') }

# release 专属编译参数：静态 CRT 与 x86-64-v3。注：只认命令行里的 --release，不靠 profile 名推断。
$IsRelease = $CargoArgs -contains '--release'
if ($IsRelease) {
    $ReleaseFlags = '-C target-feature=+crt-static -C target-cpu=x86-64-v3'
    if ($env:RUSTFLAGS) { $env:RUSTFLAGS = "$env:RUSTFLAGS $ReleaseFlags" } else { $env:RUSTFLAGS = $ReleaseFlags }
    Write-Host "== release 专属参数 (静态 CRT + x86-64-v3): RUSTFLAGS=$env:RUSTFLAGS"
}

$vcvars = Get-ChildItem "C:\Program Files\Microsoft Visual Studio\*\*\VC\Auxiliary\Build\vcvars64.bat" -ErrorAction SilentlyContinue |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $vcvars) {
    throw "找不到 vcvars64.bat，请确认已安装 Visual Studio 的 C++ 桌面开发工作负载"
}

Write-Host "== 载入 MSVC 环境: $vcvars"
$dump = cmd /c "`"$vcvars`" >nul 2>&1 && set"
foreach ($line in $dump) {
    if ($line -match '^([^=]+)=(.*)$') {
        [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
    }
}

if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    throw "MSVC 环境导入后仍找不到 link.exe"
}

$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe" }

Write-Host "== cargo $($CargoArgs -join ' ')"
& $cargo @CargoArgs
$code = $LASTEXITCODE

exit $code
