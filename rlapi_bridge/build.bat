@echo off
echo building...

:: always pull the newest rlapi from git
go get -u github.com/dank/rlapi@latest
if %ERRORLEVEL% NEQ 0 exit /b %ERRORLEVEL%

go mod vendor
if %ERRORLEVEL% NEQ 0 exit /b %ERRORLEVEL%

copy /Y _src\bridge.go vendor\github.com\dank\rlapi\bridge.go
if %ERRORLEVEL% NEQ 0 exit /b %ERRORLEVEL%

if not exist dist mkdir dist

go build -mod=vendor -o dist\rlapi-bridge.exe .
if %ERRORLEVEL% NEQ 0 exit /b %ERRORLEVEL%

echo finished: dist\rlapi-bridge.exe
