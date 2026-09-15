# limelisten 构建脚本
#
# 为什么需要它：本机装的是 Visual Studio 18 Community，而 rustc 自带的 MSVC 探测
# （vswhere/注册表）认不出它，导致 `link.exe` 找不到、`cc` 也编不了 bundled SQLite。
# 这里先导入 vcvars64 的环境再调 cargo，等价于"开发者命令提示符里跑 cargo"。
#
# 用法：  .\build.ps1              # cargo build
#         .\build.ps1 run          # cargo run
#         .\build.ps1 build --release   # 发布版（自带静态 CRT，见下）
#         .\build.ps1 slim              # 纯听版 = --release --no-default-features
#
# 静态 CRT（+crt-static）：release 产物把 C/C++ 运行库静态链进 exe，不再依赖
# vcruntime140.dll / msvcp140.dll（默认的 MSVC 目标是动态 CRT），这样绿色便携包拷到
# 没装 VC++ 运行时的机器也能直接跑。debug 构建不加，保持增量编译速度。
$ErrorActionPreference = 'Stop'

$IsSlim = $false
$CargoArgs = @($args)
if (-not $CargoArgs -or $CargoArgs.Count -eq 0) { $CargoArgs = @('build') }
if ($CargoArgs[0] -eq 'slim') {
    $IsSlim = $true
    $CargoArgs = @('build', '-p', 'lime-app', '--no-default-features', '--release')
}

# release 专属编译参数：静态 CRT。注：只认命令行里的 --release，不靠 profile 名推断，
# slim 那条路径上面已经显式带了 --release。
$IsRelease = $CargoArgs -contains '--release'
if ($IsRelease) {
    $StaticCrt = '-C target-feature=+crt-static'
    if ($env:RUSTFLAGS) { $env:RUSTFLAGS = "$env:RUSTFLAGS $StaticCrt" } else { $env:RUSTFLAGS = $StaticCrt }
    Write-Host "== release 静态 CRT: RUSTFLAGS=$env:RUSTFLAGS"
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

if ($code -eq 0 -and $IsSlim) {
    $srcExe = "target\release\limelisten.exe"
    $dstExe = "target\release\limelisten-slim.exe"
    if (Test-Path $srcExe) {
        Copy-Item -Path $srcExe -Destination $dstExe -Force
        $len = (Get-Item $dstExe).Length
        $mb = [math]::Round($len / 1MB, 2)
        Write-Host "== ✅ 已生成 Slim 独立可执行文件: $dstExe ($mb MB)" -ForegroundColor Green
    }
    # slim 和完整版共用 `target\release\limelisten.exe` 这一个产物路径，
    # 所以上面这步会把完整版覆盖成纯听版（只差 whisper 特性，文件名一模一样）。
    Write-Host "== ⚠️ 完整版已被覆盖为纯听版；要保留完整版再跑一次：.\build.ps1 build --release" -ForegroundColor Yellow
}

exit $code
