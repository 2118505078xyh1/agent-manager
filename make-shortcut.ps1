# 生成桌面快捷方式与图标。
#
# 图标是程序画的：深色圆角底 + 蓝色 "AM"。不依赖外部 png，构建时生成。
#
# 用法：powershell -File make-shortcut.ps1

Add-Type -AssemblyName System.Drawing, System.Windows.Forms

$ErrorActionPreference = 'Stop'

$exe = "$env:LOCALAPPDATA\Programs\AgentManager\agent-manager.exe"
if (-not (Test-Path $exe)) {
  throw "找不到 exe：$exe（先跑 build.bat）"
}

# ---------- 1. 画图标 ----------
function New-Icon([int]$size, [string]$out) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'
  $g.TextRenderingHint = 'AntiAliasGridFit'

  # 圆角深色底
  $bg = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 23, 24, 28))
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $r = [int]($size * 0.18)
  $d = $r * 2
  $path.AddArc(0, 0, $d, $d, 180, 90)
  $path.AddArc($size - $d, 0, $d, $d, 270, 90)
  $path.AddArc($size - $d, $size - $d, $d, $d, 0, 90)
  $path.AddArc(0, $size - $d, $d, $d, 90, 90)
  $path.CloseFigure()
  $g.FillPath($bg, $path)

  # 蓝色渐变条（左上角装饰）
  $accent = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 76, 141, 255))
  $g.FillRectangle($accent, 0, 0, [int]($size * 0.05), $size)

  # "AM"
  $font = New-Object System.Drawing.Font("Segoe UI", ($size * 0.42), [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
  $fg = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 233, 234, 238))
  $fmt = New-Object System.Drawing.StringFormat
  $fmt.Alignment = 'Center'
  $fmt.LineAlignment = 'Center'
  $rect = New-Object System.Drawing.RectangleF 0, 0, $size, $size
  $g.DrawString("AM", $font, $fg, $rect, $fmt)

  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}

$dir = "$env:LOCALAPPDATA\Programs\AgentManager"
$png256 = Join-Path $dir "icon.png"
New-Icon 256 $png256
"图标已生成：$png256"

# ---------- 2. 把 png 转成 ico（多尺寸，Windows 快捷方式要 ico）----------
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Collections.Generic;

public static class IcoMaker {
    public static void FromPng(string pngPath, string icoPath, int[] sizes) {
        using (var src = new Bitmap(pngPath)) {
            var blobs = new List<byte[]>();
            foreach (var s in sizes) {
                using (var bmp = new Bitmap(src, new Size(s, s))) {
                    using (var ms = new MemoryStream()) {
                        bmp.Save(ms, ImageFormat.Png);
                        blobs.Add(ms.ToArray());
                    }
                }
            }
            using (var fs = new FileStream(icoPath, FileMode.Create)) {
                using (var w = new BinaryWriter(fs)) {
                    w.Write((short)0); w.Write((short)1); w.Write((short)sizes.Length);
                    int offset = 6 + 16 * sizes.Length;
                    for (int i = 0; i < sizes.Length; i++) {
                        int s = sizes[i];
                        w.Write((byte)(s >= 256 ? 0 : s));
                        w.Write((byte)(s >= 256 ? 0 : s));
                        w.Write((byte)0); w.Write((byte)0);
                        w.Write((short)1); w.Write((short)32);
                        w.Write(blobs[i].Length);
                        w.Write(offset);
                        offset += blobs[i].Length;
                    }
                    foreach (var b in blobs) w.Write(b);
                }
            }
        }
    }
}
"@

$ico = Join-Path $dir "agent-manager.ico"
[IcoMaker]::FromPng($png256, $ico, @(16, 24, 32, 48, 64, 128, 256))
"ICO 已生成：$ico"

# ---------- 3. 桌面快捷方式 ----------
$desktop = [Environment]::GetFolderPath('Desktop')
$lnk = Join-Path $desktop "Agent Manager.lnk"

$ws = New-Object -ComObject WScript.Shell
$sc = $ws.CreateShortcut($lnk)
$sc.TargetPath = $exe
$sc.WorkingDirectory = $dir
$sc.IconLocation = "$ico,0"
$sc.Description = "Pi / opencode 独立管理器"
$sc.WindowStyle = 1
$sc.Save()
"快捷方式已创建：$lnk"

# ---------- 4. 开始菜单 ----------
$startDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$lnk2 = Join-Path $startDir "Agent Manager.lnk"
$sc2 = $ws.CreateShortcut($lnk2)
$sc2.TargetPath = $exe
$sc2.WorkingDirectory = $dir
$sc2.IconLocation = "$ico,0"
$sc2.Description = "Pi / opencode 独立管理器"
$sc2.Save()
"开始菜单已创建：$lnk2"