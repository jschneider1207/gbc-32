# The arithmetic of layout-set.sh (README.md "Layout sets"): reads each
# capture's last complete bench report and prints per-workload set figures.
#
#   elixir layout-set.exs summary <set dir>
#   elixir layout-set.exs compare <set dir A> <set dir B>
#
# A set dir has runs/pad<N>-<time>.txt captures (read-serial.sh output, as
# run-qspi.sh writes them; NULs and CRs as the VCP gives them). Each must
# hold a complete report (`h7bench v1 banner image=bench` .. `h7bench v1 end
# pass=N`); its workload lines give `<pass> <workload>` keys (`flash` or
# `qspi`) and the `mean=` / `max=` fields. One capture per pad (the newest
# if there are several). All captures of a set must have the same lines.
#
# spread % = (largest - smallest build mean) / set mean * 100.
# compare: delta % = (B set mean / A set mean - 1) * 100, and "noise" when
# |delta| < max(spread A, spread B), else "OUTSIDE".
#
# Exit: 0 done; 2 usage, or a capture without a complete report, or sets
# whose lines differ.

defmodule LayoutSet do
  def die(msg) do
    IO.puts(:stderr, "layout-set.exs: " <> msg)
    System.halt(2)
  end

  # %{pad => %{key => {mean, max}}} plus the key order of the first report.
  def load(dir) do
    caps =
      Path.wildcard(Path.join([dir, "runs", "pad*-*.txt"]))
      |> Enum.group_by(fn f -> f |> Path.basename() |> String.split("-") |> hd() end)
      |> Enum.map(fn {pad, files} -> {pad, files |> Enum.sort() |> List.last()} end)
      |> Enum.sort_by(fn {"pad" <> n, _} -> String.to_integer(n) end)

    if length(caps) < 2, do: die("#{dir}: fewer than two pad captures in runs/")

    builds =
      for {pad, file} <- caps do
        {pad, report(file)}
      end

    [{_, {order, _}} | _] = builds

    for {pad, {o, _}} <- builds, o != order do
      die("#{dir}: #{pad} has other workload lines than #{elem(hd(builds), 0)}")
    end

    {order, for({pad, {_, m}} <- builds, do: {pad, m})}
  end

  def report(file) do
    lines =
      File.read!(file)
      |> String.replace(["\0", "\r"], "")
      |> String.split("\n")

    # The last complete report: from the last banner that has an end line after it.
    {_, done} =
      Enum.reduce(lines, {nil, nil}, fn l, {cur, done} ->
        cond do
          String.starts_with?(l, "h7bench v1 banner image=bench ") -> {[], done}
          cur != nil and Regex.match?(~r/^h7bench v1 end pass=\d+$/, l) -> {nil, Enum.reverse(cur)}
          cur != nil -> {[l | cur], done}
          true -> {cur, done}
        end
      end)

    if done == nil, do: die("#{file}: no complete report")

    rows =
      for l <- done,
          m = Regex.run(~r/^h7bench v1 (qspi )?([^ ]+) calls=.* mean=(\d+) max=(\d+)/, l),
          m != nil do
        [_, q, w, me, mx] = m
        {(if q == "", do: "flash", else: "qspi") <> " " <> w,
         {String.to_integer(me), String.to_integer(mx)}}
      end

    if rows == [], do: die("#{file}: no workload lines in the last report")
    {Enum.map(rows, &elem(&1, 0)), Map.new(rows)}
  end

  def stats(builds, key, which) do
    v = for {_, m} <- builds, do: elem(m[key], which)
    mean = Enum.sum(v) / length(v)
    {mean, Enum.min(v), Enum.max(v), (Enum.max(v) - Enum.min(v)) / mean * 100}
  end

  def pct(x), do: :io_lib.format("~.3f", [x]) |> to_string()
  def spct(x), do: (if x >= 0, do: "+", else: "") <> pct(x)

  def summary(dir) do
    {order, builds} = load(dir)
    pads = Enum.map_join(builds, " ", &elem(&1, 0))
    IO.puts("layout-set: #{dir}: #{length(builds)} builds (#{pads})")
    IO.puts("| Line | set mean of mean | min..max | spread % | set mean of max | spread % |")
    IO.puts("|---|---|---|---|---|---|")

    worst =
      for k <- order do
        {m, lo, hi, s} = stats(builds, k, 0)
        {xm, _, _, xs} = stats(builds, k, 1)
        IO.puts("| #{k} | #{round(m)} | #{lo}..#{hi} | #{pct(s)} | #{round(xm)} | #{pct(xs)} |")
        {k, max(s, xs)}
      end
      |> Enum.max_by(&elem(&1, 1))

    IO.puts("largest spread: #{pct(elem(worst, 1))}% (#{elem(worst, 0)})")
  end

  def compare(a, b) do
    {oa, ba} = load(a)
    {ob, bb} = load(b)
    if oa != ob, do: die("#{a} and #{b} have different workload lines")
    IO.puts("layout-set: B #{b} (#{length(bb)} builds) vs A #{a} (#{length(ba)} builds)")
    IO.puts("| Line | A mean | B mean | delta % | max delta % | spread A / B % | verdict |")
    IO.puts("|---|---|---|---|---|---|---|")

    outside =
      for k <- oa do
        {ma, _, _, sa} = stats(ba, k, 0)
        {mb, _, _, sb} = stats(bb, k, 0)
        {xa, _, _, _} = stats(ba, k, 1)
        {xb, _, _, _} = stats(bb, k, 1)
        d = (mb / ma - 1) * 100
        v = if abs(d) < max(sa, sb), do: "noise", else: "OUTSIDE"

        IO.puts(
          "| #{k} | #{round(ma)} | #{round(mb)} | #{spct(d)} | #{spct((xb / xa - 1) * 100)} | #{pct(sa)} / #{pct(sb)} | #{v} |"
        )

        {k, d, v}
      end
      |> Enum.filter(fn {_, _, v} -> v == "OUTSIDE" end)

    IO.puts(
      "lines outside noise: #{length(outside)}" <>
        Enum.map_join(outside, "", fn {k, d, _} -> "; #{k} #{spct(d)}%" end)
    )
  end
end

case System.argv() do
  ["summary", dir] -> LayoutSet.summary(dir)
  ["compare", a, b] -> LayoutSet.compare(a, b)
  _ -> LayoutSet.die("usage: layout-set.exs summary <dir> | compare <dirA> <dirB>")
end
