@echo off
chcp 65001 1>nul 2>nul
set "LANG=en_US.UTF-8"
set "LANGUAGE=en_US"
set "LC_CTYPE=en_US.UTF-8"
set "LC_NUMERIC=en_US.UTF-8"
set "LC_TIME=en_US.UTF-8"
set "LC_COLLATE=en_US.UTF-8"
set "LC_MONETARY=en_US.UTF-8"
set "LC_MESSAGES=en_US.UTF-8"
set "LC_PAPER=en_US.UTF-8"
set "LC_NAME=en_US.UTF-8"
set "LC_ADDRESS=en_US.UTF-8"
set "LC_TELEPHONE=en_US.UTF-8"
set "LC_MEASUREMENT=en_US.UTF-8"
set "LC_IDENTIFICATION=en_US.UTF-8"
set "LC_ALL=en_US.UTF-8"
set "TZ=UTC"

pushd "%~sdp0"

::--------------------------------------- needs NodeJS binary in the system's PATH. should be version v20.1.0+ for `readdir`'s `recursive:true`.
where "node.exe" 1>nul 2>nul
if ["%ErrorLevel%"] neq ["0"] ( goto ERROR_NO_NODE_IN_PATH )

::------------------------------------------------------------------------ check current version is more than 20.1.0 minimum for `readdir` with `recursive`.
set "NODE_VERSION_MAJOR="
set "NODE_VERSION_MINOR="
for /F "usebackq tokens=1,2 delims=." %%A IN (`node.exe --version`) do ( 
  set "NODE_VERSION_MAJOR=%%A"
  set /a "NODE_VERSION_MINOR=%%B"
  goto EXIT_LOOP__NODE_VERSION
)
:EXIT_LOOP__NODE_VERSION

set /a "NODE_VERSION_MAJOR=%NODE_VERSION_MAJOR:v=%"

if ["%NODE_VERSION_MAJOR%"] lss ["20"] ( goto ERROR_NODE_MAJOR_LOWER_THAN_20 )

if ["%NODE_VERSION_MAJOR%"] equ ["20"] ( 
  if ["%NODE_VERSION_MINOR%"] lss ["1"] ( 
    goto ERROR_NODE_MAJOR_EQUAL_20_BUT_NODE_MINOR_LESS_THAN_1
  )
)

echo [INFO] current node version MAJOR: [%NODE_VERSION_MAJOR%]
echo [INFO] current node version MINOR: [%NODE_VERSION_MINOR%]
echo [INFO] NodeJS version is suitable for 'reddir' with 'recursive' (newer than v20.1)

node.exe "__00_format_pre.js" "%CD%"
set "EXIT_CODE=%ErrorLevel%"
if ["%EXIT_CODE%"] equ ["0"] ( 
  echo [INFO] pre fmt done.
) else ( 
  echo [INFO] pre fmt failed with exit code: [%EXIT_CODE%] - 1 generic error, 2 invalid target, 3 no files, 4 zero success no files modified.
) 

echo.
echo.
echo.
goto USE_RUST_FMT

::---------------------------------------------------------------------
:ERROR_NO_NODE_IN_PATH
echo [ERROR] NodeJS (node.exe) was not found in system PATH.
goto USE_RUST_FMT

:ERROR_NODE_MAJOR_LOWER_THAN_20
echo [ERROR] NodeJS version is not suitable for 'reddir' with 'recursive' (major lower than 20)
goto USE_RUST_FMT

:ERROR_NODE_MAJOR_EQUAL_20_BUT_NODE_MINOR_LESS_THAN_1
echo [ERROR] NodeJS version is not suitable for 'reddir' with 'recursive' (major is 20, but minor is lower than 1)
goto USE_RUST_FMT
::------------------------------------------------
:USE_RUST_FMT

::--------------------------------------- delete fmt cache.
echo [INFO] clearing rust fmt cache.
del /f /q "target\.rustfmt.cache" 1>nul 2>nul
::--------------------------------------- use rust's fmt command with rustfmt.toml explicitly.
echo.
echo.
echo.
echo [INFO] executing cargo fmt with explicit rustfmt.toml
cargo fmt --verbose --message-format human -- --config-path rustfmt.toml

timeout /t 10