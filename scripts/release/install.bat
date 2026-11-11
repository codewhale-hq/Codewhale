@echo off
setlocal
:: One guarded archive installer; optional host remains opt-in, Node default.
if not exist "%~dp0install.ps1" (
  echo ERROR: install.ps1 missing; extract the complete release archive.
  exit /b 1
)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
exit /b %ERRORLEVEL%
