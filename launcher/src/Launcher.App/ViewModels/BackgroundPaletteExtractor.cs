using System.Runtime.InteropServices;
using Avalonia;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using Avalonia.Platform;

namespace Launcher.App.ViewModels;

internal static class BackgroundPaletteExtractor
{
    private const int SampleDimension = 48;

    public static BackgroundPalette? Extract(Bitmap? source)
    {
        if (source is null)
        {
            return null;
        }

        var sourceSize = source.PixelSize;
        if (sourceSize.Width <= 0 || sourceSize.Height <= 0)
        {
            return null;
        }

        var scale = Math.Min(
            1d,
            Math.Min(
                SampleDimension / (double)sourceSize.Width,
                SampleDimension / (double)sourceSize.Height));
        var sampleSize = new PixelSize(
            Math.Max(1, (int)Math.Round(sourceSize.Width * scale)),
            Math.Max(1, (int)Math.Round(sourceSize.Height * scale)));

        using var scaled = sampleSize == sourceSize
            ? null
            : source.CreateScaledBitmap(sampleSize, BitmapInterpolationMode.MediumQuality);
        var bitmap = scaled ?? source;
        var size = bitmap.PixelSize;
        var rowBytes = checked(size.Width * 4);
        var pixels = new byte[checked(rowBytes * size.Height)];

        using (var framebuffer = new ManagedFramebuffer(size, bitmap.Dpi, rowBytes))
        {
            bitmap.CopyPixels(framebuffer);
            Marshal.Copy(framebuffer.Address, pixels, 0, pixels.Length);
        }

        var buckets = new Dictionary<int, ColorBucket>();
        var totalWeight = 0d;
        var averageRed = 0d;
        var averageGreen = 0d;
        var averageBlue = 0d;

        for (var offset = 0; offset < pixels.Length; offset += 4)
        {
            var alpha = pixels[offset + 3] / 255d;
            if (alpha < 0.60)
            {
                continue;
            }

            var blue = pixels[offset];
            var green = pixels[offset + 1];
            var red = pixels[offset + 2];
            var maximum = Math.Max(red, Math.Max(green, blue));
            var minimum = Math.Min(red, Math.Min(green, blue));
            var brightness = maximum / 255d;
            if (brightness < 0.06 || brightness > 0.98)
            {
                continue;
            }

            var saturation = maximum == 0 ? 0 : (maximum - minimum) / (double)maximum;
            var luminance = (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255d;
            var brightnessWeight = 1 - Math.Min(0.70, Math.Abs(luminance - 0.54) * 0.85);
            var weight = alpha * (0.35 + saturation * 1.65) * brightnessWeight;
            if (weight <= 0)
            {
                continue;
            }

            totalWeight += weight;
            averageRed += red * weight;
            averageGreen += green * weight;
            averageBlue += blue * weight;

            var bucketRed = red / 32;
            var bucketGreen = green / 32;
            var bucketBlue = blue / 32;
            var key = bucketRed | (bucketGreen << 3) | (bucketBlue << 6);
            if (!buckets.TryGetValue(key, out var bucket))
            {
                bucket = new ColorBucket();
                buckets[key] = bucket;
            }

            bucket.Weight += weight;
            bucket.Red += red * weight;
            bucket.Green += green * weight;
            bucket.Blue += blue * weight;
            bucket.Saturation += saturation * weight;
        }

        if (totalWeight <= 0 || buckets.Count == 0)
        {
            return null;
        }

        var dominant = Color.FromRgb(
            ToByte(averageRed / totalWeight),
            ToByte(averageGreen / totalWeight),
            ToByte(averageBlue / totalWeight));

        var meaningfulWeight = totalWeight * 0.015;
        var dominantBucket = buckets.Values
            .Where(bucket => bucket.Weight >= meaningfulWeight)
            .OrderByDescending(bucket => bucket.Weight * (0.55 + bucket.AverageSaturation * 0.90))
            .FirstOrDefault();
        if (dominantBucket is not null)
        {
            dominant = dominantBucket.ToColor();
        }

        var accentBucket = buckets.Values
            .Where(bucket => bucket.Weight >= meaningfulWeight && bucket.AverageSaturation >= 0.20)
            .OrderByDescending(bucket => bucket.Weight * (0.65 + bucket.AverageSaturation * 1.80))
            .FirstOrDefault();
        var accent = accentBucket is null
            ? (Color?)null
            : MakeReadableAccent(accentBucket.ToColor());

        // Keep the tint restrained. The source image should influence the shell,
        // not turn every surface into a bright copy of one photo pixel.
        var tint = Color.FromRgb(
            ToByte(dominant.R * 0.52),
            ToByte(dominant.G * 0.52),
            ToByte(dominant.B * 0.52));
        return new BackgroundPalette(tint, accent);
    }

    private static Color MakeReadableAccent(Color color)
    {
        var maximum = Math.Max(color.R, Math.Max(color.G, color.B));
        if (maximum > 0 && maximum < 150)
        {
            var factor = Math.Min(1.55, 150d / maximum);
            color = Scale(color, factor);
        }

        if (Luminance(color) < 0.42)
        {
            color = Blend(color, Colors.White, 0.12);
        }

        return color;
    }

    private static double Luminance(Color color) =>
        (0.2126 * color.R + 0.7152 * color.G + 0.0722 * color.B) / 255d;

    private static Color Blend(Color first, Color second, double amount)
    {
        var factor = Math.Clamp(amount, 0, 1);
        return Color.FromRgb(
            ToByte(first.R + (second.R - first.R) * factor),
            ToByte(first.G + (second.G - first.G) * factor),
            ToByte(first.B + (second.B - first.B) * factor));
    }

    private static Color Scale(Color color, double factor) => Color.FromRgb(
        ToByte(color.R * factor),
        ToByte(color.G * factor),
        ToByte(color.B * factor));

    private static byte ToByte(double value) =>
        (byte)Math.Clamp((int)Math.Round(value), 0, byte.MaxValue);

    private sealed class ColorBucket
    {
        public double Weight { get; set; }
        public double Red { get; set; }
        public double Green { get; set; }
        public double Blue { get; set; }
        public double Saturation { get; set; }
        public double AverageSaturation => Weight <= 0 ? 0 : Saturation / Weight;

        public Color ToColor() => Color.FromRgb(
            ToByte(Red / Weight),
            ToByte(Green / Weight),
            ToByte(Blue / Weight));
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

        public void Dispose() => Marshal.FreeHGlobal(Address);
    }
}

internal sealed record BackgroundPalette(Color Tint, Color? Accent);
