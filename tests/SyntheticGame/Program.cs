#if VERSION_B
const string version = "B";
#else
const string version = "A";
#endif

var markerPath = Path.Combine(Environment.CurrentDirectory, "launched.txt");
var sharedDataPath = Path.Combine(Environment.CurrentDirectory, "Data", "shared.txt");
var sharedData = File.Exists(sharedDataPath) ? await File.ReadAllTextAsync(sharedDataPath) : "missing";
await File.WriteAllTextAsync(markerPath, $"SyntheticGame {version}\nshared={sharedData}\n");
Console.WriteLine($"SyntheticGame version={version} marker={markerPath}");
