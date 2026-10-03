# Run the native Qt menu regression tests with the same Qt as the Windows GUI.
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$qtRoot = Join-Path $repoRoot 'third_party/qt'
$buildDir = Join-Path $repoRoot 'target/qml-tests'
$testDir = Join-Path $repoRoot 'crates/gui/tests'

if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vsRoot) { throw 'MSVC build tools were not found.' }
    & (Join-Path $vsRoot 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
}

New-Item -ItemType Directory -Force $buildDir | Out-Null
Push-Location $buildDir
$originalPath = $env:PATH
$originalQmlPath = $env:QML2_IMPORT_PATH
$originalPluginPath = $env:QT_PLUGIN_PATH
try {
    & (Join-Path $qtRoot 'bin/qmake.exe') (Join-Path $testDir 'qml_tests.pro')
    if ($LASTEXITCODE -ne 0) { throw 'qmake failed.' }
    & nmake /NOLOGO
    if ($LASTEXITCODE -ne 0) { throw 'Building QML tests failed.' }

    $env:PATH = (Join-Path $qtRoot 'bin') + ';' + $env:PATH
    $env:QML2_IMPORT_PATH = Join-Path $qtRoot 'qml'
    $env:QT_PLUGIN_PATH = Join-Path $qtRoot 'plugins'
    $resultFile = Join-Path $buildDir 'results.txt'
    & (Join-Path $buildDir 'release/aoproxy-qml-tests.exe') -platform offscreen -input $testDir -o "$resultFile,txt"
    $testExitCode = $LASTEXITCODE
    if (Test-Path -LiteralPath $resultFile) { Get-Content -LiteralPath $resultFile }
    if ($testExitCode -ne 0) { throw "QML tests failed (exit $testExitCode)." }
}
finally {
    $env:PATH = $originalPath
    $env:QML2_IMPORT_PATH = $originalQmlPath
    $env:QT_PLUGIN_PATH = $originalPluginPath
    Pop-Location
}
