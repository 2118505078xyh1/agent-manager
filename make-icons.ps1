# 生成三个软件的漂亮图标（纯 System.Drawing 绘制，多尺寸 ICO），并更新快捷方式。
#   代理管理  -> 蓝色：连接的节点（Agent 编排）
#   鹊桥网关  -> 青色：拱桥 + 流动光点（鹊桥 / 网关）
#   资源监控  -> 绿色：屏幕 + 心跳脉冲（ResMon）
# 用法：powershell -ExecutionPolicy Bypass -File make-icons.ps1

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$dirA = "$env:LOCALAPPDATA\Programs\AgentManager"
$dirB = "$env:LOCALAPPDATA\Programs\QueQiaoGateway"
$dirR = "$env:LOCALAPPDATA\Programs\ResMon"

# ---------- 绘图公共函数 ----------
function New-Bitmap([int]$s) {
  $bmp = New-Object System.Drawing.Bitmap $s, $s
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  return @{ bmp = $bmp; g = $g }
}

function Round-Rect([System.Drawing.Graphics]$g, [int]$s, [int]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $p.AddArc(0, 0, $d, $d, 180, 90)
  $p.AddArc($s - $d, 0, $d, $d, 270, 90)
  $p.AddArc($s - $d, $s - $d, $d, $d, 0, 90)
  $p.AddArc(0, $s - $d, $d, $d, 90, 90)
  $p.CloseFigure()
  return $p
}

function Draw-Background([System.Drawing.Graphics]$g, [int]$s, [System.Drawing.Color]$c1, [System.Drawing.Color]$c2) {
  $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush (
    (New-Object System.Drawing.PointF 0, 0),
    (New-Object System.Drawing.PointF $s, $s), $c1, $c2)
  $path = Round-Rect $g $s ([int]($s * 0.22))
  $g.FillPath($brush, $path)
  $brush.Dispose(); $path.Dispose()
}

function Save-Png([System.Drawing.Bitmap]$bmp, [string]$out) {
  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
}

# ---------- 图标 1：代理管理（连接的节点）----------
function Draw-AgentManager([int]$s, [string]$out) {
  $b = New-Bitmap $s; $g = $b.g
  Draw-Background $g $s ([System.Drawing.Color]::FromArgb(255, 40, 90, 200)) ([System.Drawing.Color]::FromArgb(255, 18, 32, 78))
  $cx = $s / 2; $cy = $s / 2
  # 连线
  $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(170, 150, 195, 255)), ([Math]::Max(1.5, $s * 0.018))
  $nodes = @(
    @{x=$cx; y=$s*0.30}, @{x=$s*0.28; y=$s*0.62}, @{x=$s*0.72; y=$s*0.62}, @{x=$cx; y=$s*0.74}
  )
  $g.DrawLine($pen, [single]$nodes[0].x, [single]$nodes[0].y, [single]$nodes[1].x, [single]$nodes[1].y)
  $g.DrawLine($pen, [single]$nodes[0].x, [single]$nodes[0].y, [single]$nodes[2].x, [single]$nodes[2].y)
  $g.DrawLine($pen, [single]$nodes[1].x, [single]$nodes[1].y, [single]$nodes[3].x, [single]$nodes[3].y)
  $g.DrawLine($pen, [single]$nodes[2].x, [single]$nodes[2].y, [single]$nodes[3].x, [single]$nodes[3].y)
  # 节点
  for ($i = 0; $i -lt $nodes.Count; $i++) {
    $r = if ($i -eq 0) { $s * 0.11 } else { $s * 0.085 }
    $x = [single]$nodes[$i].x - [single]$r; $y = [single]$nodes[$i].y - [single]$r
    $col = if ($i -eq 0) { [System.Drawing.Color]::FromArgb(255, 255, 255, 255) } else { [System.Drawing.Color]::FromArgb(255, 130, 180, 255) }
    $nb = New-Object System.Drawing.SolidBrush $col
    $g.FillEllipse($nb, $x, $y, [single]$r*2, [single]$r*2)
    $nb.Dispose()
    $eb = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 25, 55, 130))
    $ir = $r * 0.42
    $g.FillEllipse($eb, [single]$nodes[$i].x - [single]$ir, [single]$nodes[$i].y - [single]$ir, [single]$ir*2, [single]$ir*2)
    $eb.Dispose()
  }
  $pen.Dispose()
  Save-Png $b.bmp $out; $g.Dispose(); $b.bmp.Dispose()
}

# ---------- 图标 2：鹊桥网关（喜鹊 + 星河/桥）----------
function Draw-QueQiao([int]$s, [string]$out) {
  $b = New-Bitmap $s; $g = $b.g
  Draw-Background $g $s ([System.Drawing.Color]::FromArgb(255, 22, 150, 160)) ([System.Drawing.Color]::FromArgb(255, 8, 56, 78))

  # 背景星点（银河）
  $stars = @(@{x=0.20;y=0.22;r=0.014},@{x=0.78;y=0.18;r=0.011},@{x=0.86;y=0.42;r=0.013},@{x=0.14;y=0.52;r=0.010},@{x=0.66;y=0.20;r=0.009})
  $starBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 200, 255, 250))
  foreach ($st in $stars) {
    $r = [single]($s * $st.r)
    $g.FillEllipse($starBrush, [single]($s*$st.x)-$r, [single]($s*$st.y)-$r, $r*2, $r*2)
  }
  $starBrush.Dispose()

  # 星河弧线（鹊桥）
  $arcPen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(150, 190, 255, 250)), ([Math]::Max(1.5, [int]($s*0.016)))
  $arcRect = New-Object System.Drawing.Rectangle ([int]($s*0.10), [int]($s*0.12), [int]($s*0.80), [int]($s*0.50))
  $g.DrawArc($arcPen, $arcRect, 200, 140)
  $arcPen.Dispose()

  $black = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 34, 42, 56))
  $blueBlack = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 44, 60, 96))
  $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 245, 250, 255))
  $beak = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 230, 190, 90))

  # 尾羽（长，蓝黑，向右下）
  $tail = @(
    (New-Object System.Drawing.Point ([int]($s*0.58),[int]($s*0.60))),
    (New-Object System.Drawing.Point ([int]($s*0.88),[int]($s*0.44))),
    (New-Object System.Drawing.Point ([int]($s*0.90),[int]($s*0.56))),
    (New-Object System.Drawing.Point ([int]($s*0.60),[int]($s*0.70)))
  )
  $g.FillPolygon($blueBlack, $tail)

  # 身体（黑背 + 白腹）
  $g.FillEllipse($black, [single]($s*0.28), [single]($s*0.46), [single]($s*0.40), [single]($s*0.30))
  $g.FillEllipse($white, [single]($s*0.30), [single]($s*0.55), [single]($s*0.34), [single]($s*0.20))

  # 翅膀（深色 + 白斑）
  $wing = New-Object System.Drawing.Drawing2D.GraphicsPath
  $wing.AddEllipse([single]($s*0.34), [single]($s*0.50), [single]($s*0.26), [single]($s*0.16))
  $g.FillPath($blueBlack, $wing)
  $wing.Dispose()
  $g.FillEllipse($white, [single]($s*0.48), [single]($s*0.53), [single]($s*0.10), [single]($s*0.045))

  # 头（黑） + 白脸颊
  $g.FillEllipse($black, [single]($s*0.22), [single]($s*0.28), [single]($s*0.24), [single]($s*0.24))
  $g.FillEllipse($white, [single]($s*0.30), [single]($s*0.36), [single]($s*0.14), [single]($s*0.12))

  # 喙（三角）
  $beakPts = @(
    (New-Object System.Drawing.Point ([int]($s*0.22),[int]($s*0.38))),
    (New-Object System.Drawing.Point ([int]($s*0.10),[int]($s*0.34))),
    (New-Object System.Drawing.Point ([int]($s*0.22),[int]($s*0.45)))
  )
  $g.FillPolygon($beak, $beakPts)

  # 眼睛
  $g.FillEllipse($white, [single]($s*0.31), [single]($s*0.36), [single]($s*0.045), [single]($s*0.045))
  $eye = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 20, 24, 32))
  $g.FillEllipse($eye, [single]($s*0.325), [single]($s*0.375), [single]($s*0.022), [single]($s*0.022))
  $eye.Dispose()

  # 腿
  $legPen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 230, 190, 90)), ([Math]::Max(1.5, [int]($s*0.014)))
  $g.DrawLine($legPen, [single]($s*0.40), [single]($s*0.74), [single]($s*0.40), [single]($s*0.84))
  $g.DrawLine($legPen, [single]($s*0.50), [single]($s*0.74), [single]($s*0.50), [single]($s*0.84))
  $legPen.Dispose()

  $black.Dispose(); $blueBlack.Dispose(); $white.Dispose(); $beak.Dispose()
  Save-Png $b.bmp $out; $g.Dispose(); $b.bmp.Dispose()
}

# ---------- 图标 3：资源监控（屏幕 + 心跳）----------
function Draw-ResMon([int]$s, [string]$out) {
  $b = New-Bitmap $s; $g = $b.g
  Draw-Background $g $s ([System.Drawing.Color]::FromArgb(255, 40, 170, 110)) ([System.Drawing.Color]::FromArgb(255, 10, 58, 52))
  # 屏幕外框
  $framePen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 225, 255, 240)), ([Math]::Max(2, [int]($s * 0.035)))
  $frame = New-Object System.Drawing.Rectangle ([int]($s*0.14), [int]($s*0.22), [int]($s*0.72), [int]($s*0.52))
  $g.DrawRectangle($framePen, $frame)
  # 支架
  $g.DrawLine($framePen, [single]($s*0.5), [single]($s*0.74), [single]($s*0.5), [single]($s*0.82))
  $g.DrawLine($framePen, [single]($s*0.36), [single]($s*0.84), [single]($s*0.64), [single]($s*0.84))
  # 心跳折线
  $pulse = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 130, 255, 170)), ([Math]::Max(2, [int]($s * 0.032)))
  $pts = @(
    (New-Object System.Drawing.Point ([int]($s*0.20),[int]($s*0.50))),
    (New-Object System.Drawing.Point ([int]($s*0.33),[int]($s*0.50))),
    (New-Object System.Drawing.Point ([int]($s*0.39),[int]($s*0.38))),
    (New-Object System.Drawing.Point ([int]($s*0.46),[int]($s*0.64))),
    (New-Object System.Drawing.Point ([int]($s*0.53),[int]($s*0.32))),
    (New-Object System.Drawing.Point ([int]($s*0.60),[int]($s*0.50))),
    (New-Object System.Drawing.Point ([int]($s*0.80),[int]($s*0.50)))
  )
  $g.DrawLines($pulse, $pts)
  $framePen.Dispose(); $pulse.Dispose()
  Save-Png $b.bmp $out; $g.Dispose(); $b.bmp.Dispose()
}

# ---------- PNG -> 多尺寸 ICO ----------
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Collections.Generic;
public static class IcoMaker3 {
  public static void FromPng(string png, string ico, int[] sizes) {
    using (var src = new Bitmap(png)) {
      var blobs = new List<byte[]>();
      foreach (var s in sizes) {
        using (var b = new Bitmap(src, new Size(s, s))) using (var ms = new MemoryStream()) {
          b.Save(ms, ImageFormat.Png); blobs.Add(ms.ToArray());
        }
      }
      using (var fs = new FileStream(ico, FileMode.Create)) using (var w = new BinaryWriter(fs)) {
        w.Write((short)0); w.Write((short)1); w.Write((short)sizes.Length);
        int off = 6 + 16 * sizes.Length;
        for (int i=0;i<sizes.Length;i++) {
          int s=sizes[i]; w.Write((byte)(s>=256?0:s)); w.Write((byte)(s>=256?0:s));
          w.Write((byte)0); w.Write((byte)0); w.Write((short)1); w.Write((short)32);
          w.Write(blobs[i].Length); w.Write(off); off += blobs[i].Length;
        }
        foreach (var blob in blobs) w.Write(blob);
      }
    }
  }
}
"@

function Build-Icon([string]$dir, [string]$name, [scriptblock]$drawer) {
  $png = Join-Path $dir ($name + ".png")
  $ico = Join-Path $dir ($name + ".ico")
  & $drawer 256 $png
  [IcoMaker3]::FromPng($png, $ico, @(16, 24, 32, 48, 64, 128, 256))
  Write-Host "生成图标：$ico"
  return $ico
}

$icoA = Build-Icon $dirA "agent-manager" ${function:Draw-AgentManager}
$icoB = Build-Icon $dirB "queqiao-gateway" ${function:Draw-QueQiao}
$icoR = Build-Icon $dirR "resmon" ${function:Draw-ResMon}

# ---------- 更新快捷方式 ----------
function Set-ShortcutIcon([string]$baseName, [string]$ico) {
  $desktop = [Environment]::GetFolderPath('Desktop')
  $start = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
  $ws = New-Object -ComObject WScript.Shell
  foreach ($lnk in @((Join-Path $desktop "$baseName.lnk"), (Join-Path $start "$baseName.lnk"))) {
    if (Test-Path $lnk) {
      $sc = $ws.CreateShortcut($lnk)
      $sc.IconLocation = "$ico,0"
      $sc.Save()
      Write-Output "更新图标：$lnk"
    }
  }
}

Set-ShortcutIcon "代理管理" $icoA
Set-ShortcutIcon "鹊桥网关" $icoB
Set-ShortcutIcon "资源监控" $icoR

Write-Output "完成。"
