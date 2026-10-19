@echo off
cargo build --release
if %errorlevel%==0 (
    copy /Y target\release\json-forge.exe json-forge.exe >nul
    echo Build successful: json-forge.exe
) else (
    echo Build failed!
)
pause
