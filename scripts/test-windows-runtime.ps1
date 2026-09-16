param([string]$Executable = 'target/release/gbf-power-reborn.exe')
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$Stage = Join-Path $Root ('artifacts/runtime-test-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Stage | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class GbfRuntimeDialog {
  public delegate bool Callback(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumWindows(Callback callback, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, Callback callback, IntPtr p);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder text, int count);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder text, int count);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  public static IntPtr Find(int pid) {
    IntPtr result = IntPtr.Zero;
    EnumWindows((h,p) => { uint owner; GetWindowThreadProcessId(h,out owner);
      var name = new StringBuilder(128); GetClassName(h,name,name.Capacity);
      if (owner == pid && name.ToString() == "#32770") { result=h; return false; } return true;
    },IntPtr.Zero);
    return result;
  }
  public static string Text(IntPtr h) {
    var parts = new List<string>();
    EnumChildWindows(h,(child,p)=> { var text=new StringBuilder(4096); GetWindowText(child,text,text.Capacity); parts.Add(text.ToString()); return true; },IntPtr.Zero);
    return String.Join("\n",parts);
  }
}
'@
try {
  $App = Join-Path $Stage 'GBF POWER REBORN.exe'
  Copy-Item -LiteralPath $Executable -Destination $App
  # BOM intentionally exercises Windows PowerShell 5 packaging compatibility.
  [IO.File]::WriteAllText((Join-Path $Stage 'manifest.json'),'{"package":"portable_webview2"}',[Text.UTF8Encoding]::new($true))
  foreach ($Case in @('missing', 'incomplete')) {
    if ($Case -eq 'incomplete') {
      New-Item -ItemType Directory -Path (Join-Path $Stage 'WebView2Runtime') | Out-Null
      [IO.File]::WriteAllText((Join-Path $Stage 'WebView2Runtime/msedgewebview2.exe'),'invalid fixture')
    }
    $Child = Start-Process -FilePath $App -WorkingDirectory $env:TEMP -WindowStyle Hidden -PassThru
    $null = $Child.Handle
    try {
      $Deadline = [DateTime]::UtcNow.AddSeconds(10)
      $Dialog = [IntPtr]::Zero
      while ($Dialog -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $Deadline -and !$Child.HasExited) {
        $Dialog = [GbfRuntimeDialog]::Find($Child.Id)
        if ($Dialog -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 100 }
      }
      if ($Dialog -eq [IntPtr]::Zero) { throw "No native Runtime error dialog: $Case" }
      $Text = [GbfRuntimeDialog]::Text($Dialog)
      if ($Text -notlike '*WebView2 Runtime*' -or $Text -notlike '*portable_webview2*') { throw "Unexpected Runtime error: $Text" }
      [GbfRuntimeDialog]::SendMessage($Dialog,0x10,[IntPtr]::Zero,[IntPtr]::Zero) | Out-Null
      $Exited = $Child.WaitForExit(5000)
      if (!$Exited -or $Child.ExitCode -ne 1) { throw "Runtime error did not exit with failure: exited=$Exited code=$($Child.ExitCode)" }
      "PASS: $Case Fixed Runtime displays native repair message before accessing application data."
    } finally {
      if (!$Child.HasExited) { $Child.Kill(); $Child.WaitForExit() }
      $Child.Dispose()
    }
  }
} finally {
  $Resolved = [IO.Path]::GetFullPath($Stage)
  if ([IO.Path]::GetDirectoryName($Resolved) -ne [IO.Path]::GetFullPath((Join-Path $Root 'artifacts')) -or [IO.Path]::GetFileName($Resolved) -notlike 'runtime-test-*') { throw 'Unsafe Runtime test cleanup path.' }
  Remove-Item -LiteralPath $Resolved -Recurse -Force
}
