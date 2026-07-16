@echo off
setlocal EnableExtensions
pushd "%~dp0\.." || exit /b 1
set "LOG=windows-ci-current-2.log"
set "RESULT=windows-ci-current-2-result.txt"
set "CARGO_TARGET_DIR=C:\vosu-target"
set "GUARD_LOG=windows-negative-release-guard.log"
set "RC=1"
>"%LOG%" echo WINDOWS CI START %DATE% %TIME%
>"%RESULT%" echo RUNNING
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%i"
if not defined VSROOT goto fail
call "%VSROOT%\VC\Auxiliary\Build\vcvars64.bat" || goto fail
cargo fmt --all -- --check >>"%LOG%" 2>&1 || goto fail
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings >>"%LOG%" 2>&1 || goto fail
cargo test --workspace --all-targets --all-features --locked >>"%LOG%" 2>&1 || goto fail
cargo test --locked --test process_memory -- --nocapture >>"%LOG%" 2>&1 || goto fail
cargo test --locked --test updater_native -- --nocapture >>"%LOG%" 2>&1 || goto fail
cargo build --release --locked --no-default-features --target x86_64-pc-windows-msvc >>"%LOG%" 2>&1 || goto fail
cargo build --release --locked --features test-harness --target x86_64-pc-windows-msvc >"%GUARD_LOG%" 2>&1
if not errorlevel 1 goto guard_fail
findstr /c:"the test-harness feature is forbidden in release builds" "%GUARD_LOG%" >nul || goto guard_fail
type "%GUARD_LOG%" >>"%LOG%"
>"%RESULT%" echo PASS
set "RC=0"
goto publish
:guard_fail
>>"%LOG%" echo FAIL negative release guard
:fail
>"%RESULT%" echo FAIL
:publish
type "%RESULT%"
popd
exit /b %RC%
