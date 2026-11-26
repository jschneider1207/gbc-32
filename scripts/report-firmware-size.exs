# gbc-core's share of a linked firmware ELF, against the H735 ITCM gate
# (docs/perf.md "Firmware-link size"). Report only: exits 0 unless a tool or
# an input fails (exit 2).
#
#   elixir scripts/report-firmware-size.exs <elf> [--rlib <libgbc_core.rlib>]
#
# <elf>: a firmware image that links gbc-core, e.g.
#   crates/gbc-nucleo-h753/out/headless-cpu_instrs.elf (build.sh).
# --rlib: the gbc-core rlib the image was linked from (build.sh leaves it at
#   crates/gbc-nucleo-h753/target/thumbv7em-none-eabihf/release/build/
#   gbc-core/*/out/). Adds the rlib totals (the figure that
#   scripts/check-code-size.sh gates), what the link dropped, and the
#   anonymous .rodata of gbc-core (named by its CGU hash).
#
# Tools: readelf and ar (binutils) and an ARM objdump: $ARM_OBJDUMP, else
# arm-none-eabi-objdump on PATH, else the Arm GNU toolchain under
# ~/.local/opt (docs/reviews/stage1-m10/C-size-mcu.md).
#
# What it measures:
#   - gbc-core code: every FUNC symbol whose v0 path is rooted in the
#     `gbc_core` crate (its own functions and their generic instantiations;
#     the size includes literal pools and in-function alignment nops),
#     split by region (ITCM below 0x0002_0000, else flash).
#   - The closure: every function and data object reachable from the
#     gbc-core functions through direct branches, movw/movt address pairs,
#     and 4-aligned words (literal pools, vtables, panic Locations) that
#     hold a function's or an object's start address. Everything in it that
#     is not gbc-core (core's panic and fmt code, compiler_builtins, the
#     callee side of trait objects) would have to sit in ITCM too for
#     "all of gbc-core in ITCM" to run zero-wait. Over-approximate: a data
#     word that happens to equal an address adds a node.
#   - Padding: the gaps between consecutive functions of an executable
#     section, each counted against the function after it (its alignment).
#   - Veneers: lld's long-branch thunks (`__Thumbv7ABSLongThunk_*`).
#   - The closure split: the normal path stops at the panic entry points
#     (core::panicking, *_fail, unwrap/expect failures, the panic handler)
#     and at gbc-core's own core::fmt impls; the rest is the panic/fmt
#     path, which can stay in flash behind veneers (counted, 10 B each).
#     Scenarios A-D at the end put these against the gate.
#   - gbc-core .rodata in the link: named objects rooted in gbc_core plus
#     the `.Lanon.<hash>` objects of gbc-core's CGU (with --rlib).

defmodule FwSize do
  @itcm_end 0x0002_0000
  @gate 122_880
  @itcm 131_072

  def main(args) do
    {opts, rest, _} = OptionParser.parse(args, strict: [rlib: :string])

    elf =
      case rest do
        [e] -> e
        _ -> die("usage: report-firmware-size.exs <elf> [--rlib <libgbc_core.rlib>]")
      end

    File.exists?(elf) || die("no ELF at #{elf}")
    objdump = find_objdump()
    bytes = File.read!(elf)
    sections = read_sections(elf)
    syms = read_symbols(elf, sections)
    rlib = if opts[:rlib], do: read_rlib(opts[:rlib]), else: nil

    funcs = syms |> Enum.filter(&(&1.type == "FUNC" and &1.size > 0 and &1.exec))
    objs = syms |> Enum.filter(&(&1.type == "OBJECT" and &1.size > 0 and not &1.exec))

    by_start =
      Map.new(funcs ++ objs, fn s -> {s.start, s.id} end)

    nodes = Map.new(funcs ++ objs, fn s -> {s.id, s} end)

    # Edges from code: branch targets and movw/movt pairs from the
    # disassembly, plus 4-aligned words in the function's bytes.
    code_edges = disasm_edges(objdump, elf, sections, funcs, by_start)

    word_edges =
      Map.new(funcs ++ objs, fn s ->
        {s.id, words(bytes, sections, s) |> Enum.flat_map(&lookup(by_start, &1)) |> MapSet.new()}
      end)

    edges =
      Map.merge(word_edges, code_edges, fn _k, a, b -> MapSet.union(a, b) end)

    core? = fn s -> crate(s.name) == "gbc_core" and not thunk?(s.name) end
    roots = funcs |> Enum.filter(core?) |> Enum.map(& &1.id)
    barrier = funcs |> Enum.filter(&panic_entry?(&1.name)) |> MapSet.new(& &1.id)
    reach = closure(roots, edges, MapSet.new())
    # The same closure, stopped at the panic entry points: what the core
    # runs when nothing panics.
    hot = closure(roots, edges, barrier)

    core_funcs = Enum.filter(funcs, core?)
    thunks = Enum.filter(funcs, &thunk?(&1.name))
    gaps = gaps(funcs, sections)
    as_funcs = fn set -> set |> Enum.map(&nodes[&1]) |> Enum.filter(&(&1.type == "FUNC")) end
    other? = fn s -> not core?.(s) and not thunk?(s.name) end

    hot_extra = hot |> MapSet.difference(barrier) |> as_funcs.() |> Enum.filter(other?)
    hot_ids = MapSet.new(hot_extra, & &1.id)

    panic_extra =
      reach |> as_funcs.() |> Enum.filter(other?) |> Enum.reject(&MapSet.member?(hot_ids, &1.id))

    panic_ids = MapSet.new(panic_extra, & &1.id)

    # Veneers if the panic path stays in flash: one per panic-path function
    # that ITCM code (gbc-core or the hot extras) references directly.
    itcm_ids = Enum.map(core_funcs, & &1.id) ++ Enum.map(hot_extra, & &1.id)

    veneer_targets =
      itcm_ids
      |> Enum.flat_map(&MapSet.to_list(Map.get(code_edges, &1, MapSet.new())))
      |> Enum.filter(&MapSet.member?(panic_ids, &1))
      |> Enum.uniq()

    thunk_size = thunks |> Enum.map(& &1.size) |> Enum.max(fn -> 10 end)
    core_hash = rlib && rlib.anon_hash

    core_ro? = fn s ->
      crate(s.name) == "gbc_core" or
        (core_hash != nil and String.starts_with?(s.name, ".Lanon.#{core_hash}."))
    end

    reach_objs = reach |> Enum.map(&nodes[&1]) |> Enum.filter(&(&1.type == "OBJECT"))
    pad = fn l -> l |> Enum.map(&Map.get(gaps, &1.id, 0)) |> Enum.sum() end

    IO.puts("Firmware-link size of gbc-core: #{elf}")
    IO.puts("(report only; docs/perf.md \"Firmware-link size\")\n")

    if rlib do
      linked = MapSet.new(funcs, & &1.name)
      dropped = rlib.funcs |> Enum.reject(&MapSet.member?(linked, &1.name))
      rooted = MapSet.new(core_funcs, & &1.name)

      foreign =
        rlib.funcs
        |> Enum.filter(&MapSet.member?(linked, &1.name))
        |> Enum.reject(&MapSet.member?(rooted, &1.name))

      elsewhere = core_funcs |> Enum.reject(&MapSet.member?(rlib.func_names, &1.name))

      IO.puts("rlib (#{opts[:rlib]}):")
      line("  .text*, every section (the check-code-size.sh gate)", rlib.text)
      line("  FUNC symbols (#{length(rlib.funcs)})", sum(rlib.funcs))
      line("  - not in the link (unused, GC'd) (#{length(dropped)})", sum(dropped))

      line(
        "  linked functions of the rlib not rooted in gbc_core (#{length(foreign)})",
        sum(foreign)
      )

      line(
        "  gbc_core-rooted functions emitted by other crates (#{length(elsewhere)})",
        sum(elsewhere)
      )

      line("  .rodata*", rlib.rodata)
      IO.puts("")
    end

    {ci, cf} = Enum.split_with(core_funcs, &(&1.start < @itcm_end))
    IO.puts("gbc-core functions in the link (v0 path rooted in gbc_core, #{length(core_funcs)}):")
    line("  in ITCM (#{length(ci)})", sum(ci))
    line("  in flash (#{length(cf)})", sum(cf))
    line("  total", sum(core_funcs))

    line(
      "  of which in-function nops (alignment inside the sizes)",
      core_nops(objdump, elf, sections, core_funcs)
    )

    line("  inter-function padding before them (as linked)", pad.(core_funcs))

    IO.puts("\nNon-gbc-core code the core reaches (calls, address pairs, pointers in data):")
    group(hot_extra, "  normal path")
    line("  = normal path (#{length(hot_extra)})", sum(hot_extra))
    group(panic_extra, "  panic/fmt path only")
    line("  = panic/fmt path only (#{length(panic_extra)})", sum(panic_extra))
    line("  padding before these (as linked)", pad.(hot_extra ++ panic_extra))

    IO.puts("\nNormal-path extras:")

    hot_extra
    |> Enum.sort_by(&(-&1.size))
    |> Enum.each(fn s -> line("  #{short(s.name)}", s.size) end)

    IO.puts("\nLargest panic/fmt-path extras:")

    panic_extra
    |> Enum.sort_by(&(-&1.size))
    |> Enum.take(8)
    |> Enum.each(fn s -> line("  #{short(s.name)}", s.size) end)

    {ti, tf} = Enum.split_with(thunks, &(&1.start < @itcm_end))
    IO.puts("\nVeneers (long-branch thunks, #{thunk_size} B) in this image:")
    line("  in ITCM (#{length(ti)})", sum(ti))
    line("  in flash (#{length(tf)})", sum(tf))

    IO.puts("\nInter-function padding per section (all code):")

    sections
    |> Enum.filter(& &1.exec)
    |> Enum.each(fn sec ->
      line("  #{sec.name}", funcs |> Enum.filter(&(&1.shndx == sec.idx)) |> pad.())
    end)

    IO.puts("\ngbc-core read-only data in the link:")

    objs
    |> Enum.filter(core_ro?)
    |> Enum.group_by(&section_name(sections, &1.shndx))
    |> Enum.each(fn {sec, l} -> line("  #{sec} (#{length(l)} objects)", sum(l)) end)

    if core_hash == nil, do: IO.puts("  (anonymous constants not attributed: pass --rlib)")

    reach_objs
    |> Enum.reject(core_ro?)
    |> Enum.group_by(&section_name(sections, &1.shndx))
    |> Enum.each(fn {sec, l} ->
      line("  + other crates' objects the core reaches, #{sec} (#{length(l)})", sum(l))
    end)

    code = sum(core_funcs) + pad.(core_funcs)
    hot_b = sum(hot_extra) + pad.(hot_extra)
    panic_b = sum(panic_extra) + pad.(panic_extra)
    ven = length(veneer_targets) * thunk_size

    IO.puts("\nH735 ITCM (128 KiB; gate #{@gate} B = 128 KiB minus the 8 KiB platform reserve):")
    line("  gbc-core code + padding (this link)", code)
    line("  + normal-path extras + padding", hot_b)
    line("  + veneers to a flash-resident panic/fmt path (#{length(veneer_targets)})", ven)
    a = code + hot_b + ven
    line("  = A: gbc-core + normal path in ITCM, panic/fmt path in flash", a)
    line("    margin to the gate", @gate - a)
    b = code + hot_b + panic_b
    line("  = B: the whole closure in ITCM (no veneers)", b)
    line("    margin to the gate", @gate - b)
    line("    margin to the whole ITCM", @itcm - b)

    if rlib do
      d = rlib.text + hot_b + ven
      line("  = C: rlib .text (every core function kept) + normal path, panic/fmt in flash", d)
      line("    margin to the gate", @gate - d)
      line("    margin to the whole ITCM", @itcm - d)
      w = rlib.text + hot_b + panic_b
      line("  = D: rlib .text (every core function kept) + the whole closure", w)
      line("    margin to the gate", @gate - w)
      line("    margin to the whole ITCM", @itcm - w)
    end
  end

  defp group(l, label) do
    l
    |> Enum.group_by(&crate(&1.name))
    |> Enum.sort_by(fn {_, l} -> -sum(l) end)
    |> Enum.each(fn {c, l} -> line("#{label}: #{c} (#{length(l)})", sum(l)) end)
  end

  # Where the core leaves its normal path for diagnostics: core's
  # panicking module, the out-of-line index/unwrap failures, the panic
  # handler, and the core's own `core::fmt` trait impls (`Debug` for error
  # types: a front end formats them, the emulation never does).
  defp panic_entry?(n) do
    String.starts_with?(n, "_R") and
      (String.contains?(n, "9panicking") or Regex.match?(~r/4core3fmt\d+[A-Za-z]+3fmt/, n) or
         String.contains?(n, "_fail") or
         String.contains?(n, "unwrap_failed") or String.contains?(n, "expect_failed") or
         String.contains?(n, "rust_begin_unwind"))
  end

  # ---- inputs

  defp find_objdump do
    home = System.get_env("HOME", "")

    [
      System.get_env("ARM_OBJDUMP"),
      System.find_executable("arm-none-eabi-objdump"),
      Path.wildcard(Path.join(home, ".local/opt/arm-gnu-toolchain-*/bin/arm-none-eabi-objdump"))
      |> List.last()
    ]
    |> Enum.find(&(&1 && File.exists?(&1))) || die("no ARM objdump (set ARM_OBJDUMP)")
  end

  defp run(cmd, args) do
    case System.cmd(cmd, args, stderr_to_stdout: false) do
      {out, 0} -> out
      {_, rc} -> die("#{cmd} #{Enum.join(args, " ")} failed (#{rc})")
    end
  end

  defp read_sections(elf) do
    run("readelf", ["-SW", elf])
    |> String.split("\n")
    |> Enum.flat_map(fn l ->
      case Regex.run(
             ~r/^\s*\[\s*(\d+)\]\s+(\S+)\s+(\S+)\s+([0-9a-f]{8})\s+([0-9a-f]{6,})\s+([0-9a-f]{6,})\s+\S+\s+([A-Z]*)\s/,
             l
           ) do
        [_, i, n, t, a, o, s, f] ->
          [
            %{
              idx: String.to_integer(i),
              name: n,
              nobits: t == "NOBITS",
              addr: hex(a),
              off: hex(o),
              size: hex(s),
              exec: String.contains?(f, "X"),
              alloc: String.contains?(f, "A")
            }
          ]

        _ ->
          []
      end
    end)
  end

  defp read_symbols(elf, sections) do
    secs = Map.new(sections, &{&1.idx, &1})

    run("readelf", ["-sW", elf])
    |> String.split("\n")
    |> Enum.with_index()
    |> Enum.flat_map(fn {l, n} ->
      case String.split(l) do
        [_, v, sz, t, _bind, _vis, ndx, name] ->
          with {i, ""} <- Integer.parse(ndx), %{alloc: true} = s <- secs[i] do
            size =
              if String.starts_with?(sz, "0x"),
                do: hex(String.slice(sz, 2..-1//1)),
                else: String.to_integer(sz)

            [
              %{
                id: n,
                name: name,
                type: t,
                start: Bitwise.band(hex(v), Bitwise.bnot(1)),
                size: size,
                shndx: i,
                exec: s.exec
              }
            ]
          else
            _ -> []
          end

        _ ->
          []
      end
    end)
    |> Enum.uniq_by(&{&1.name, &1.start})
  end

  defp read_rlib(path) do
    File.exists?(path) || die("no rlib at #{path}")
    dir = Path.join(System.tmp_dir!(), "fwsize-#{System.unique_integer([:positive])}")
    File.mkdir_p!(dir)

    try do
      {_, 0} = System.cmd("ar", ["x", Path.expand(path)], cd: dir)
      objs = Path.wildcard(Path.join(dir, "*.o"))
      objs != [] || die("no objects in #{path}")
      size_out = run("size", ["-A" | objs])

      {text, rodata} =
        size_out
        |> String.split("\n")
        |> Enum.reduce({0, 0}, fn l, {t, r} ->
          case String.split(l) do
            [n, s, _] ->
              cond do
                String.starts_with?(n, ".text") -> {t + String.to_integer(s), r}
                String.starts_with?(n, ".rodata") -> {t, r + String.to_integer(s)}
                true -> {t, r}
              end

            _ ->
              {t, r}
          end
        end)

      syms = Enum.flat_map(objs, fn o -> read_obj_funcs(o) end)

      hash =
        Enum.find_value(objs, fn o ->
          case Regex.run(~r/\.Lanon\.([0-9a-f]+)\./, run("readelf", ["-sW", o])) do
            [_, h] -> h
            _ -> nil
          end
        end)

      %{
        text: text,
        rodata: rodata,
        funcs: syms,
        func_names: MapSet.new(syms, & &1.name),
        anon_hash: hash
      }
    after
      File.rm_rf!(dir)
    end
  end

  defp read_obj_funcs(o) do
    run("readelf", ["-sW", o])
    |> String.split("\n")
    |> Enum.flat_map(fn l ->
      case String.split(l) do
        [_, _, sz, "FUNC", _, _, _, name] -> [%{name: name, size: String.to_integer(sz)}]
        _ -> []
      end
    end)
  end

  # ---- analysis

  defp disasm(objdump, elf, sections) do
    args =
      ["-d", "--no-show-raw-insn"] ++
        (sections |> Enum.filter(& &1.exec) |> Enum.flat_map(&["-j", &1.name])) ++ [elf]

    run(objdump, args)
  end

  # Per function start: the set of node ids its instructions reference.
  defp disasm_edges(objdump, elf, sections, funcs, by_start) do
    starts = Map.new(funcs, &{&1.start, &1.id})

    {edges, _, _} =
      disasm(objdump, elf, sections)
      |> String.split("\n")
      |> Enum.reduce({%{}, nil, %{}}, fn l, {acc, cur, regs} ->
        cond do
          m = Regex.run(~r/^([0-9a-f]{8}) <.*>:$/, l) ->
            {acc, Map.get(starts, hex(Enum.at(m, 1)), cur), %{}}

          cur == nil ->
            {acc, cur, regs}

          m = Regex.run(~r/^\s*[0-9a-f]+:\t(\S+)\t(.*)$/, l) ->
            [_, mn, ops] = m

            cond do
              Regex.match?(~r/^(bl|blx|b|b[a-z]{2})(\.[wn])?$/, mn) ->
                case Regex.run(~r/^([0-9a-f]+) </, ops) do
                  [_, t] -> {add(acc, cur, lookup(by_start, hex(t))), cur, regs}
                  _ -> {acc, cur, regs}
                end

              mn in ["movw", "movt"] ->
                case Regex.run(~r/^(\w+), #(\d+)/, ops) do
                  [_, r, imm] ->
                    imm = String.to_integer(imm)

                    if mn == "movw" do
                      {acc, cur, Map.put(regs, r, imm)}
                    else
                      v = Bitwise.bor(Bitwise.bsl(imm, 16), Map.get(regs, r, 0))
                      {add(acc, cur, lookup(by_start, v)), cur, Map.delete(regs, r)}
                    end

                  _ ->
                    {acc, cur, regs}
                end

              true ->
                {acc, cur, regs}
            end

          true ->
            {acc, cur, regs}
        end
      end)

    edges
  end

  defp core_nops(objdump, elf, sections, core_funcs) do
    set = MapSet.new(core_funcs, & &1.start)

    {n, _} =
      disasm(objdump, elf, sections)
      |> String.split("\n")
      |> Enum.reduce({0, false}, fn l, {n, in_core} ->
        cond do
          m = Regex.run(~r/^([0-9a-f]{8}) <.*>:$/, l) ->
            {n, MapSet.member?(set, hex(Enum.at(m, 1)))}

          in_core and Regex.match?(~r/^\s*[0-9a-f]+:\tnop\.w\b/, l) ->
            {n + 4, in_core}

          in_core and Regex.match?(~r/^\s*[0-9a-f]+:\tnop\b/, l) ->
            {n + 2, in_core}

          true ->
            {n, in_core}
        end
      end)

    n
  end

  defp add(acc, _cur, []), do: acc

  defp add(acc, cur, ids),
    do: Map.update(acc, cur, MapSet.new(ids), &MapSet.union(&1, MapSet.new(ids)))

  defp lookup(by_start, v) do
    case Map.get(by_start, Bitwise.band(v, Bitwise.bnot(1))) do
      nil -> []
      id -> [id]
    end
  end

  defp words(_bytes, _sections, %{size: sz}) when sz < 4, do: []

  defp words(bytes, sections, s) do
    sec = Enum.find(sections, &(&1.idx == s.shndx))

    if sec.nobits do
      []
    else
      first = Bitwise.band(s.start + 3, Bitwise.bnot(3))
      last = s.start + s.size - 4

      if first > last,
        do: [],
        else:
          for(
            a <- first..last//4,
            do:
              (
                <<w::little-32>> = binary_part(bytes, sec.off + a - sec.addr, 4)
                w
              )
          )
    end
  end

  # Every node reachable from the roots; a node in `stop` is included but
  # not followed.
  defp closure(roots, edges, stop) do
    do_closure(roots, MapSet.new(roots), edges, stop)
  end

  defp do_closure([], seen, _, _), do: seen

  defp do_closure([h | t], seen, edges, stop) do
    new =
      if MapSet.member?(stop, h),
        do: [],
        else: edges |> Map.get(h, MapSet.new()) |> Enum.reject(&MapSet.member?(seen, &1))

    do_closure(new ++ t, Enum.reduce(new, seen, &MapSet.put(&2, &1)), edges, stop)
  end

  # Gap before each function (to the end of the previous one in its section).
  defp gaps(funcs, sections) do
    sections
    |> Enum.filter(& &1.exec)
    |> Enum.flat_map(fn sec ->
      fs = funcs |> Enum.filter(&(&1.shndx == sec.idx)) |> Enum.sort_by(& &1.start)

      {pairs, _} =
        Enum.map_reduce(fs, sec.addr, fn f, prev_end ->
          {{f.id, max(f.start - prev_end, 0)}, max(prev_end, f.start + f.size)}
        end)

      pairs
    end)
    |> Map.new()
  end

  # ---- names

  defp thunk?(n), do: String.starts_with?(n, "__") and String.contains?(n, "Thunk_")

  # The crate a symbol belongs to: the first crate root of a v0 name
  # (`C[s<disambiguator>_]<len><ident>`), the first segment of a legacy
  # name, `compiler_builtins` for the C-ABI helpers, else `(unmangled)`.
  def crate(name) do
    name = Regex.replace(~r/^__\w*Thunk_/, name, "")

    cond do
      String.starts_with?(name, "_R") ->
        case Regex.run(~r/C(?:s[0-9A-Za-z]*_)?(\d+)/, name,
               capture: :all_but_first,
               return: :index
             ) do
          [{p, l}] ->
            len = String.to_integer(binary_part(name, p, l))
            rest = binary_part(name, p + l, byte_size(name) - p - l)

            rest =
              if String.starts_with?(rest, "_"),
                do: binary_part(rest, 1, byte_size(rest) - 1),
                else: rest

            binary_part(rest, 0, min(len, byte_size(rest)))

          _ ->
            "(v0, no crate)"
        end

      String.starts_with?(name, "_ZN") ->
        case Regex.run(~r/^_ZN(\d+)/, name) do
          [_, d] -> binary_part(name, 3 + byte_size(d), String.to_integer(d))
          _ -> "(legacy)"
        end

      Regex.match?(~r/^(__aeabi_|__a?eabi|mem(cpy|set|move|cmp)$|__(u|)div|__.*[sd]f\d)/, name) ->
        "compiler_builtins"

      true ->
        "(unmangled)"
    end
  end

  defp short(n) do
    case System.cmd("c++filt", ["--format=rust", n]) do
      {out, 0} -> out |> String.trim() |> String.slice(0, 70)
      _ -> String.slice(n, 0, 70)
    end
  end

  defp section_name(sections, idx), do: Enum.find(sections, &(&1.idx == idx)).name
  defp sum(l), do: l |> Enum.map(& &1.size) |> Enum.sum()
  defp hex(s), do: String.to_integer(s, 16)

  defp line(label, v),
    do:
      IO.puts(
        String.pad_trailing(label, 78) <> String.pad_leading(Integer.to_string(v), 9) <> " B"
      )

  defp die(msg) do
    IO.puts(:stderr, "report-firmware-size: #{msg}")
    System.halt(2)
  end
end

FwSize.main(System.argv())
