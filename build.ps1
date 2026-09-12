# limelisten 构建脚本
#
# 为什么需要它：本机装的是 Visual Studio 18 Community，而 rustc 自带的 MSVC 探测
# （vswhere/注册表）认不出它，导致 `link.exe` 找不到、`cc` 也编不了 bundled SQLite。
# 这里先导入 vcvars64 的环境再调 cargo，等价于"开发者命令提示符里跑 cargo"。
#
# 用法：  .\build.ps1              # cargo build
#         .\build.ps1 run          # cargo run
#         .\build.ps1 build --release
$ErrorActionPreference = 'Stop'

$IsSlim = $false
$CargoArgs = @($args)
if (-not $CargoArgs -or $CargoArgs.Count -eq 0) { $CargoArgs = @('build') }
if ($CargoArgs[0] -eq 'slim') {
    $IsSlim = $true
    $CargoArgs = @('build', '-p', 'lime-app', '--no-default-features', '--release')
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
}

exit $code
