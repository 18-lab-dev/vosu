using System.Reflection;
using System.Reflection.Emit;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace LazerOffsetGenerator;

/// <summary>
/// Resolves managed instance-field offsets from on-disk assemblies without attaching
/// to a live CLR. Used when Satori/live DAC attach is unavailable.
/// </summary>
internal static class AssemblyOffsets
{
    public static byte[] Generate(string assemblyDirectory, TemplateSchema template, string? osuVersion)
    {
        string directory = Path.GetFullPath(assemblyDirectory);
        if (!Directory.Exists(directory))
        {
            throw new GeneratorException($"assembly directory '{directory}' does not exist");
        }

        foreach (string path in Directory.EnumerateFiles(directory, "*.dll"))
        {
            try
            {
                _ = Assembly.LoadFrom(path);
            }
            catch (Exception)
            {
                // Ignore unloadable dependency graphs; required game types usually load.
            }
        }

        Dictionary<string, Type> runtimeTypes = AppDomain.CurrentDomain.GetAssemblies()
            .SelectMany(static assembly =>
            {
                try
                {
                    return assembly.GetTypes();
                }
                catch (ReflectionTypeLoadException error)
                {
                    return error.Types.Where(static type => type is not null).Cast<Type>();
                }
                catch (Exception)
                {
                    return Type.EmptyTypes;
                }
            })
            .Where(static type => type.FullName is not null)
            .GroupBy(static type => type.FullName!, StringComparer.Ordinal)
            .ToDictionary(static group => group.Key, static group => group.First(), StringComparer.Ordinal);

        string version = osuVersion
            ?? ReadOsuVersion(directory)
            ?? throw new GeneratorException(
                $"could not determine osu! version from assembly directory '{directory}'");

        JsonObject root = [];
        foreach (TemplateEntry entry in template.Entries)
        {
            switch (entry.Kind)
            {
                case TemplateEntryKind.OsuVersion:
                    root[entry.Name] = version;
                    break;
                case TemplateEntryKind.GameBaseVtable:
                    // Match the live generator's first MethodTable word, not its
                    // ASLR-dependent address. Rehosting uses the client's CoreCLR.
                    Type gameType = ResolveRuntimeType(TemplateSchema.GameVtableTypeName, runtimeTypes);
                    root[entry.Name] = System.Runtime.InteropServices.Marshal.ReadInt64(gameType.TypeHandle.Value);
                    break;
                case TemplateEntryKind.ClrType:
                    root[entry.Name] = ResolveTypeFields(entry, runtimeTypes);
                    break;
            }
        }

        return JsonSerializer.SerializeToUtf8Bytes(
            root,
            new JsonSerializerOptions
            {
                WriteIndented = false,
                Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
            });
    }

    private static JsonObject ResolveTypeFields(
        TemplateEntry entry,
        IReadOnlyDictionary<string, Type> runtimeTypes)
    {
        Type declared = ResolveRuntimeType(entry.Name, runtimeTypes);
        Type concrete = FindConcreteType(declared, runtimeTypes)
            ?? throw new GeneratorException(
                $"could not find a concrete runtime type assignable to '{entry.Name}'");

        JsonObject fields = [];
        foreach (string fieldName in entry.Fields)
        {
            FieldInfo field = FindField(declared, fieldName)
                ?? throw new GeneratorException(
                    $"could not resolve field '{entry.Name}.{fieldName}'");
            int offset = MeasureFieldOffset(declared, concrete, field);
            if (offset <= 0)
            {
                throw new GeneratorException(
                    $"measured non-positive offset {offset} for '{entry.Name}.{fieldName}'");
            }

            fields[fieldName] = offset;
        }

        return fields;
    }

    private static Type ResolveRuntimeType(
        string typeName,
        IReadOnlyDictionary<string, Type> runtimeTypes)
    {
        if (runtimeTypes.TryGetValue(typeName, out Type? exact))
            return exact;

        // CLRMD names constructed generics using angle brackets. Resolve the same
        // names for offline measurement rather than measuring an open generic.
        int bracket = typeName.IndexOf('<');
        if (bracket > 0 && typeName.EndsWith('>'))
        {
            string argument = typeName[(bracket + 1)..^1];
            Type argumentType = argument.EndsWith("[]", StringComparison.Ordinal)
                ? ResolveRuntimeType(argument[..^2], runtimeTypes).MakeArrayType()
                : ResolveRuntimeType(argument, runtimeTypes);
            Type definition = runtimeTypes[typeName[..bracket] + "`1"];
            return definition.MakeGenericType(argumentType);
        }

        Type? generic = runtimeTypes.Values.FirstOrDefault(type =>
            type.FullName == typeName
            || type.FullName?.StartsWith(typeName + "`", StringComparison.Ordinal) == true);
        if (generic is not null)
            return generic;

        throw new GeneratorException($"could not resolve runtime type '{typeName}' from assemblies");
    }

    private static Type? FindConcreteType(Type declared, IReadOnlyDictionary<string, Type> runtimeTypes)
    {
        if (declared is { IsAbstract: false, IsInterface: false, ContainsGenericParameters: false })
            return declared;

        return runtimeTypes.Values
            .Where(type =>
                type is { IsClass: true, IsAbstract: false, ContainsGenericParameters: false }
                && IsAssignableToDeclared(declared, type))
            .OrderBy(static type => type.FullName?.Length ?? int.MaxValue)
            .FirstOrDefault();
    }

    private static bool IsAssignableToDeclared(Type declared, Type concrete)
    {
        try
        {
            return declared.IsAssignableFrom(concrete);
        }
        catch (Exception)
        {
            // Generic open types can throw during assignability checks across load contexts.
            return concrete.BaseType is not null
                && (concrete.BaseType == declared
                    || concrete.BaseType.FullName == declared.FullName
                    || (declared.IsGenericTypeDefinition
                        && concrete.BaseType.IsGenericType
                        && concrete.BaseType.GetGenericTypeDefinition() == declared));
        }
    }

    private static FieldInfo? FindField(Type declared, string fieldName)
    {
        const BindingFlags flags =
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly;
        for (Type? current = declared; current is not null; current = current.BaseType)
        {
            FieldInfo? field = current.GetField(fieldName, flags);
            if (field is not null)
                return field;
        }

        return declared.GetField(
            fieldName,
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
    }

    private static int MeasureFieldOffset(Type declared, Type concrete, FieldInfo field)
    {
        object instance;
        try
        {
            instance = RuntimeHelpers.GetUninitializedObject(concrete);
        }
        catch (Exception error)
        {
            throw new GeneratorException(
                $"could not allocate uninitialized '{concrete.FullName}' while measuring '{field.Name}': {error.Message}",
                error);
        }
        // No constructor ran, so a finalizer (for example osu.Game.Skinning.Skin's
        // Dispose(false)) would dereference null fields on the finalizer thread and
        // crash the generator whenever a GC happens to collect the instance.
        GC.SuppressFinalize(instance);

        Type owner = field.DeclaringType ?? declared;
        DynamicMethod method = new(
            "FieldOffset_" + declared.Name + "_" + field.Name,
            typeof(int),
            [typeof(object)],
            owner.Module,
            skipVisibility: true);
        ILGenerator il = method.GetILGenerator();
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Castclass, owner);
        il.Emit(OpCodes.Ldflda, field);
        il.Emit(OpCodes.Conv_U);
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Conv_U);
        il.Emit(OpCodes.Sub);
        il.Emit(OpCodes.Conv_I4);
        il.Emit(OpCodes.Ret);

        try
        {
            Func<object, int> fn = method.CreateDelegate<Func<object, int>>();
            return fn(instance);
        }
        catch (Exception error)
        {
            throw new GeneratorException(
                $"could not measure offset for '{declared.FullName}.{field.Name}' via '{concrete.FullName}': {error.Message}",
                error);
        }
    }

    private static string? ReadOsuVersion(string directory)
    {
        string deps = Path.Combine(directory, "osu!.deps.json");
        if (!File.Exists(deps))
            return null;

        using JsonDocument document = JsonDocument.Parse(File.ReadAllText(deps));
        if (!document.RootElement.TryGetProperty("libraries", out JsonElement libraries))
            return null;

        foreach (JsonProperty property in libraries.EnumerateObject())
        {
            if (!property.Name.StartsWith("osu!/", StringComparison.Ordinal))
                continue;

            string version = property.Name["osu!/".Length..];
            return version.EndsWith("-lazer", StringComparison.Ordinal)
                ? version[..^"-lazer".Length]
                : version;
        }

        return null;
    }
}
