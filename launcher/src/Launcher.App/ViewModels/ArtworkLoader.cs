using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using Avalonia;
using Avalonia.Media.Imaging;
using Avalonia.Platform;

namespace Launcher.App.ViewModels;

internal static class ArtworkLoader
{
    private static readonly ConcurrentDictionary<string, Bitmap> Cache = new(StringComparer.OrdinalIgnoreCase);
    private static readonly ConcurrentDictionary<string, Bitmap> SidebarIconCache = new(StringComparer.OrdinalIgnoreCase);

    private const byte BlackBackgroundThreshold = 42;
    private const byte BlackBackgroundFeatherThreshold = 78;

    public static Bitmap? Load(string? source)
    {
        var path = ResolveLocalPath(source);
        if (path is null || !File.Exists(path)) return null;
        if (Cache.TryGetValue(path, out var cached)) return cached;

        try
        {
            using var stream = File.OpenRead(path);
            var bitmap = new Bitmap(stream);
            return Cache.GetOrAdd(path, bitmap);
        }
        catch (Exception) when (source is not null)
        {
            // Artwork is optional. A stale or unsupported Steam cache entry
            // must leave the monogram fallback usable.
            return null;
        }
    }

    public static Bitmap? LoadSidebarIcon(string? source)
    {
        var path = ResolveLocalPath(source);
        if (path is null || !File.Exists(path)) return null;
        if (SidebarIconCache.TryGetValue(path, out var cached)) return cached;

        try
        {
            var bitmap = Load(source);
            if (bitmap is null) return null;

            Bitmap icon;
            try
            {
                icon = RemoveOpaqueBlackBackground(bitmap);
            }
            catch
            {
                // If a platform decoder cannot expose pixels for an unusual image,
                // keep the original artwork rather than replacing it with a blank icon.
                icon = bitmap;
            }

            if (SidebarIconCache.TryAdd(path, icon)) return icon;

            if (!ReferenceEquals(icon, bitmap))
            {
                icon.Dispose();
            }

            return SidebarIconCache[path];
        }
        catch (Exception) when (source is not null)
        {
            // Keep a broken or unusual Steam image from hiding the monogram fallback.
            return null;
        }
    }

    private static Bitmap RemoveOpaqueBlackBackground(Bitmap source)
    {
        var size = source.PixelSize;
        var width = size.Width;
        var height = size.Height;
        var pixelCount = (long)width * height;

        // Sidebar icons are small. Do not spend large amounts of memory processing a
        // full library banner if Steam falls back to one of those files.
        if (width <= 0 || height <= 0 || pixelCount > 4_000_000)
        {
            return source;
        }

        var rowBytes = checked(width * 4);
        var pixels = new byte[checked((int)(pixelCount * 4))];
        using (var framebuffer = new ManagedFramebuffer(size, source.Dpi, rowBytes))
        {
            source.CopyPixels(framebuffer);
            Marshal.Copy(framebuffer.Address, pixels, 0, pixels.Length);
        }

        var background = new bool[checked((int)pixelCount)];
        var queue = new Queue<int>();

        for (var y = 0; y < height; y++)
        {
            EnqueueIfBackground(y * width, pixels, background, queue);
            if (height > 1)
            {
                EnqueueIfBackground(y * width + width - 1, pixels, background, queue);
            }
        }

        for (var x = 1; x < width - 1; x++)
        {
            EnqueueIfBackground(x, pixels, background, queue);
            if (height > 1)
            {
                EnqueueIfBackground((height - 1) * width + x, pixels, background, queue);
            }
        }

        var removedPixels = 0;
        while (queue.Count > 0)
        {
            var index = queue.Dequeue();
            removedPixels++;
            var x = index % width;
            var y = index / width;

            for (var offsetY = -1; offsetY <= 1; offsetY++)
            {
                for (var offsetX = -1; offsetX <= 1; offsetX++)
                {
                    if (offsetX == 0 && offsetY == 0) continue;

                    var nextX = x + offsetX;
                    var nextY = y + offsetY;
                    if ((uint)nextX >= (uint)width || (uint)nextY >= (uint)height) continue;

                    var next = nextY * width + nextX;
                    if (background[next]) continue;

                    if (IsBlackBackgroundPixel(pixels, next * 4))
                    {
                        background[next] = true;
                        queue.Enqueue(next);
                    }
                }
            }
        }

        // A few isolated dark edge pixels are usually part of the logo itself. A
        // real square backplate produces a connected region larger than this.
        if (removedPixels < Math.Max(8, Math.Min(width, height) / 2))
        {
            return source;
        }

        for (var index = 0; index < background.Length; index++)
        {
            if (!background[index]) continue;

            var offset = index * 4;
            var brightness = Math.Max(pixels[offset], Math.Max(pixels[offset + 1], pixels[offset + 2]));
            var alpha = pixels[offset + 3];
            if (brightness <= BlackBackgroundThreshold)
            {
                pixels[offset + 3] = 0;
            }
            else
            {
                var feather = (brightness - BlackBackgroundThreshold)
                    / (float)(BlackBackgroundFeatherThreshold - BlackBackgroundThreshold);
                pixels[offset + 3] = (byte)Math.Clamp((int)Math.Round(alpha * feather), 0, 255);
            }
        }

        var result = new WriteableBitmap(size, source.Dpi, PixelFormat.Bgra8888, AlphaFormat.Unpremul);
        using (var target = result.Lock())
        {
            for (var y = 0; y < height; y++)
            {
                Marshal.Copy(
                    pixels,
                    y * rowBytes,
                    IntPtr.Add(target.Address, y * target.RowBytes),
                    rowBytes);
            }
        }

        return result;
    }

    private static void EnqueueIfBackground(int index, byte[] pixels, bool[] background, Queue<int> queue)
    {
        if (background[index] || !IsBlackBackgroundPixel(pixels, index * 4)) return;

        background[index] = true;
        queue.Enqueue(index);
    }

    private static bool IsBlackBackgroundPixel(byte[] pixels, int offset)
    {
        if (pixels[offset + 3] < 224) return false;

        var blue = pixels[offset];
        var green = pixels[offset + 1];
        var red = pixels[offset + 2];
        var brightness = Math.Max(red, Math.Max(green, blue));
        var spread = Math.Max(red, Math.Max(green, blue)) - Math.Min(red, Math.Min(green, blue));
        return brightness <= BlackBackgroundFeatherThreshold && spread <= 28;
    }

    private sealed class ManagedFramebuffer : ILockedFramebuffer
    {
        public ManagedFramebuffer(PixelSize size, Vector dpi, int rowBytes)
        {
            Size = size;
            Dpi = dpi;
            RowBytes = rowBytes;
            Address = Marshal.AllocHGlobal(checked(rowBytes * size.Height));
        }

        public IntPtr Address { get; }
        public PixelSize Size { get; }
        public int RowBytes { get; }
        public Vector Dpi { get; }
        public PixelFormat Format => PixelFormat.Bgra8888;
        public AlphaFormat AlphaFormat => AlphaFormat.Unpremul;

        public void Dispose()
        {
            Marshal.FreeHGlobal(Address);
        }
    }

    private static string? ResolveLocalPath(string? source)
    {
        if (string.IsNullOrWhiteSpace(source)) return null;
        if (Path.IsPathRooted(source)) return Path.GetFullPath(source);

        return Uri.TryCreate(source, UriKind.Absolute, out var uri) && uri.IsFile
            ? uri.LocalPath
            : null;
    }
}
