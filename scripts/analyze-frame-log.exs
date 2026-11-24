# Summarises a GBC_FRAME_LOG CSV (docs/desktop-diagnostics.md).
#
#   elixir scripts/analyze-frame-log.exs frames.csv [--skip N] [--list N] [--window S]
#
# --skip N   ignore the first N frames (window creation, audio start-up;
#            default 120, about 2 s)
# --list N   list at most N irregular frames (default 20)
# --window S per-window table every S seconds (default 10), to score
#            parts of a run (e.g. screen-slide transitions)
#
# Headline: display irregularities per minute. A presented picture should
# stay on screen r = refresh rate / emulated frame rate refreshes, i.e.
# floor(r) or ceil(r) of them; on a 60 Hz display at 59.73 frames/s that
# is 1, with a 2 every ~3.7 s (the "beat": the display outruns the game,
# unavoidable without changing the game's speed). Irregular = frames shown
# outside {floor(r), ceil(r)} (e.g. 0 or 3+ at 60 Hz), plus ceil(r) frames
# beyond the number the beat explains, plus completed pictures the loop
# never presented. Excluded: blank (LCD off) frames and any picture whose
# successor was not the very next frame (an LCD-off stretch follows).
# On-screen counts come from DWM timing and are estimates.

defmodule FrameLog do
  # Game time per frame: 70224 clocks at 4194304 Hz.
  @frame_s 70224 / 4_194_304

  def main(argv) do
    {opts, files, _} = OptionParser.parse(argv, strict: [skip: :integer, list: :integer, window: :integer])

    case files do
      [path] -> run(path, Keyword.get(opts, :skip, 120), Keyword.get(opts, :list, 20), Keyword.get(opts, :window, 10))
      _ -> IO.puts(:stderr, "usage: elixir scripts/analyze-frame-log.exs frames.csv [--skip N] [--list N] [--window S]")
           System.halt(2)
    end
  end

  defp run(path, skip, list, window) do
    {comments, rows} = load(path)
    if rows == [], do: raise("no rows in #{path}")
    rate = header_rate(comments)
    all = rows
    rows = Enum.drop(rows, skip)
    if rows == [], do: raise("all #{length(all)} rows skipped (--skip #{skip})")

    t0 = hd(rows).emu_start_us
    t1 = List.last(rows).emu_end_us
    secs = (t1 - t0) / 1.0e6
    mins = secs / 60
    # The pacing rate: mean interval between consecutive frames that both
    # completed a picture (LCD-off stretches end with short partial frames).
    pairs =
      rows
      |> Enum.chunk_every(2, 1, :discard)
      |> Enum.filter(fn [a, b] -> b.frame == a.frame + 1 and a.ready and b.ready and not a.blank and not b.blank end)
      |> Enum.map(fn [a, b] -> b.emu_end_us - a.emu_end_us end)

    # Stalls (startup, a blocked loop) are not the pacing rate.
    pairs = if pairs == [], do: [], else: Enum.filter(pairs, &(&1 < 2 * median(pairs)))

    emu_fps = if pairs == [], do: (length(rows) - 1) / max(secs, 1.0e-9), else: 1.0e6 / mean(pairs)

    periods = rows |> Enum.map(& &1.refresh_period_us) |> Enum.reject(&is_nil/1)
    refresh_hz =
      cond do
        rate -> rate
        periods != [] -> 1.0e6 / median(periods)
        true -> nil
      end

    IO.puts("frame log: #{path}")
    IO.puts("  rows #{length(all)} (#{skip} skipped), #{fmt(secs, 1)} s analysed, rom #{rom(comments)}")
    IO.puts("  emulated #{fmt(emu_fps, 3)} frames/s; display refresh " <>
              if(refresh_hz, do: "#{fmt(refresh_hz, 3)} Hz (DWM)", else: "unknown (no DWM data: not Windows?)"))

    blank = Enum.count(rows, & &1.blank)
    ready = Enum.filter(rows, &(&1.ready and not &1.blank))
    windowed = Enum.any?(rows, &(&1.presents > 0))
    unpresented = if windowed, do: Enum.filter(ready, &(&1.presents == 0)), else: []
    IO.puts("  pictures #{length(ready)}, blank frames #{blank}" <>
              if(windowed, do: ", completed but never presented #{length(unpresented)}", else: " (headless: no presents)"))

    intervals(rows)
    wake(rows)

    game = game_side(rows, list)
    game_mins = length(rows) * @frame_s / 60

    disp =
      if refresh_hz && Enum.any?(rows, & &1.on_screen) do
        display(rows, refresh_hz, emu_fps, mins, unpresented, list, game)
      else
        IO.puts("\nno on-screen estimates (DWM data: native Windows window only): display metric unavailable")
        nil
      end

    windows(rows, game, disp, window)

    IO.puts("\nHEADLINE game-side: scroll irregularities #{fmt(length(game.irregular) / max(game_mins, 1.0e-9), 2)} " <>
              "/min of game time (#{length(game.irregular)} steps off the usual step in #{length(game.segments)} scrolls)")

    if disp do
      IO.puts("HEADLINE display-side: display irregularities #{fmt(disp.irregular / max(mins, 1.0e-9), 2)} /min " <>
                "(#{fmt(disp.irregular, 1)} in #{fmt(mins, 2)} min, #{fmt(disp.in_scroll, 1)} during scrolls; " <>
                "beat #{fmt(disp.beat, 1)} /min excluded)")
    end

    marks(all)
  end

  # ---- game side ---------------------------------------------------------

  # Scrolls: runs of pictures that moved along one axis in one direction
  # (`motion_dx`/`motion_dy`, derived from the frame buffers), allowing up
  # to 2 still pictures inside a run, at least 8 pictures long. A smooth
  # scroll moves the same step every frame; a step that differs from the
  # run's most common step (a 0 inside the run, or a catch-up 8 after a
  # 0) is a game-side irregularity: the picture itself moved unevenly,
  # whatever the display does.
  defp game_side(rows, list) do
    segs = segments(rows) |> Enum.filter(&(length(&1.frames) >= 8))

    scored =
      Enum.map(segs, fn seg ->
        steps = Enum.map(seg.frames, &step(&1, seg.axis))
        mode = steps |> Enum.reject(&(&1 == 0)) |> Enum.frequencies() |> Enum.max_by(&elem(&1, 1)) |> elem(0)
        # At 1 px a step, a 0 is how a game scrolls slower than 1 px per
        # frame (e.g. 1,0,1,0 = 0.5 px): counted apart, not as irregular.
        slow = abs(mode) == 1
        odd = for {r, st} <- Enum.zip(seg.frames, steps), st != mode, not (slow and st == 0), do: {r.frame, st}
        still = if slow, do: Enum.count(steps, &(&1 == 0)), else: 0
        Map.merge(seg, %{mode: mode, odd: odd, still: still})
      end)

    irregular = Enum.flat_map(scored, & &1.odd)
    in_scroll = scored |> Enum.flat_map(fn s -> Enum.map(s.frames, & &1.frame) end) |> MapSet.new()

    IO.puts("\ngame side (picture motion from the frame buffers; #{length(scored)} scrolls of 8+ pictures, " <>
              "#{MapSet.size(in_scroll)} pictures)")

    for s <- Enum.take(scored, list) do
      first = hd(s.frames).frame
      last = List.last(s.frames).frame
      odd = if s.odd == [], do: "even", else: "uneven: " <> Enum.map_join(Enum.take(s.odd, 12), " ", fn {f, st} -> "f#{f}=#{st}" end)
      odd = if length(s.odd) > 12, do: odd <> " ... (#{length(s.odd)})", else: odd
      odd = if s.still > 0, do: odd <> "; #{s.still} still (under 1 px/frame)", else: odd
      IO.puts("  f#{first}-#{last} d#{s.axis} step #{s.mode} x#{length(s.frames)}: #{odd}")
    end

    if length(scored) > list, do: IO.puts("  ... #{length(scored) - list} more")
    %{segments: scored, irregular: Enum.map(irregular, &elem(&1, 0)), in_scroll: in_scroll}
  end

  defp step(r, :x), do: r.motion_dx
  defp step(r, :y), do: r.motion_dy

  defp dir(r) do
    cond do
      r.motion_dx == nil -> nil
      r.motion_dx != 0 -> {:x, sign(r.motion_dx)}
      r.motion_dy != 0 -> {:y, sign(r.motion_dy)}
      true -> :still
    end
  end

  defp sign(v) when v > 0, do: 1
  defp sign(_), do: -1

  defp segments(rows) do
    {segs, cur, _} =
      Enum.reduce(rows, {[], nil, []}, fn r, {segs, cur, zeros} ->
        case {dir(r), cur} do
          {nil, _} -> {close(segs, cur), nil, []}
          {:still, nil} -> {segs, nil, []}
          {:still, c} when length(zeros) >= 2 -> {close(segs, c), nil, []}
          {:still, c} -> {segs, c, zeros ++ [r]}
          {{ax, sg}, %{axis: ax, sign: sg} = c} -> {segs, %{c | frames: c.frames ++ zeros ++ [r]}, []}
          {{ax, sg}, c} -> {close(segs, c), %{axis: ax, sign: sg, frames: [r]}, []}
        end
      end)

    Enum.reverse(close(segs, cur))
  end

  defp close(segs, nil), do: segs
  defp close(segs, c), do: [c | segs]

  # ---- display ---------------------------------------------------------

  defp display(rows, hz, fps, _mins, unpresented, list, game) do
    r = hz / fps
    lo = trunc(:math.floor(r))
    hi = trunc(:math.ceil(r))
    presented = Enum.filter(rows, &(&1.presents > 0))
    nexts = Enum.zip(presented, tl(presented) ++ [nil])

    eligible =
      for {row, nxt} <- nexts,
          nxt != nil,
          row.ready and not row.blank,
          row.on_screen != nil,
          nxt.frame == row.frame + 1,
          do: row

    sc = score(eligible, unpresented, lo, hi, r)
    in_scroll = score(
      Enum.filter(eligible, &MapSet.member?(game.in_scroll, &1.frame)),
      Enum.filter(unpresented, &MapSet.member?(game.in_scroll, &1.frame)),
      lo, hi, r)
    beat = abs(hz - fps) * 60

    IO.puts("\ndisplay side (per presented picture followed by the next frame's; #{length(eligible)} pictures)")
    IO.puts("  refreshes per frame r = #{fmt(r, 4)}: ideal #{if lo == hi, do: lo, else: "#{lo} or #{hi}"}")
    IO.puts("  on-screen refreshes: " <> counts(sc.dist))
    IO.puts("  beat (refresh vs emulated rate): #{fmt(beat, 1)} /min expected; " <>
              "#{hi}-refresh pictures #{Map.get(sc.dist, hi, 0)}, beat explains #{fmt(sc.hi_expected, 1)}")
    IO.puts("  irregular: #{length(sc.outside)} outside {#{lo},#{hi}}, #{fmt(sc.excess_hi, 1)} excess #{hi}s, " <>
              "#{length(unpresented)} never presented")
    IO.puts("  during scrolls: on-screen " <> counts(in_scroll.dist) <> ", irregular #{fmt(in_scroll.irregular, 1)}")
    phases(eligible, sc.outside)

    shown = Enum.take(sc.outside, list)
    if shown != [] do
      IO.puts("  first #{length(shown)} outside {#{lo},#{hi}}: " <>
                Enum.map_join(shown, ", ", &"f#{&1.frame}=#{&1.on_screen}@#{fmt(&1.emu_end_us / 1.0e6, 2)}s"))
    end

    %{irregular: sc.irregular, in_scroll: in_scroll.irregular, beat: beat, eligible: eligible,
      unpresented: unpresented, lo: lo, hi: hi, r: r}
  end

  defp score(eligible, unpresented, lo, hi, r) do
    n = length(eligible)
    dist = Enum.frequencies_by(eligible, & &1.on_screen)
    outside = Enum.filter(eligible, &(&1.on_screen < lo or &1.on_screen > hi))
    n_hi = if hi > lo, do: Map.get(dist, hi, 0), else: 0
    hi_expected = if hi > lo, do: (r - lo) * n, else: 0.0
    excess_hi = max(0.0, n_hi - hi_expected)
    %{dist: dist, outside: outside, hi_expected: hi_expected, excess_hi: excess_hi,
      irregular: length(outside) + excess_hi + length(unpresented)}
  end

  defp counts(dist), do: dist |> Enum.sort() |> Enum.map_join(" ", fn {k, v} -> "#{k}:#{v}" end)

  # Per `secs`-second window of game time (frame numbers): scroll pictures
  # and game-side irregularities; with a window, display-side
  # irregularities (scored as in the headline), the share of frames whose
  # picture changed, and the mean wake lateness.
  defp windows(rows, game, disp, secs) do
    per = round(secs / @frame_s)
    f0 = hd(rows).frame
    key = &div(&1.frame - f0, per)
    fkey = &div(&1 - f0, per)
    all = Enum.group_by(rows, key)
    odd = Enum.frequencies_by(game.irregular, fkey)
    scroll = Enum.frequencies_by(game.in_scroll, fkey)
    el = if disp, do: Enum.group_by(disp.eligible, key), else: %{}
    un = if disp, do: Enum.group_by(disp.unpresented, key), else: %{}

    IO.puts("\nper #{secs} s of game time:   start  frames  scroll  game-irr  on-screen          disp-irr  changed%  wake-late-ms")

    for {k, rs} <- Enum.sort(all) do
      {cnt, irr} =
        if disp do
          sc = score(Map.get(el, k, []), Map.get(un, k, []), disp.lo, disp.hi, disp.r)
          {counts(sc.dist), fmt(sc.irregular, 1)}
        else
          {"-", "-"}
        end

      changed = 100 * Enum.count(rs, & &1.changed) / length(rs)
      late = rs |> Enum.map(& &1.wake_late_us) |> Enum.reject(&is_nil/1)
      late = if late == [], do: "-", else: fmt(mean(late) / 1000, 3)

      IO.puts("  " <> String.pad_leading("#{k * secs}s", 26) <> "  " <>
                String.pad_leading("#{length(rs)}", 6) <> "  " <>
                String.pad_leading("#{Map.get(scroll, k, 0)}", 6) <> "  " <>
                String.pad_leading("#{Map.get(odd, k, 0)}", 8) <> "  " <>
                String.pad_trailing(cnt, 17) <> "  " <>
                String.pad_leading(irr, 8) <> "  " <>
                String.pad_leading(fmt(changed, 0), 8) <> "  " <> late)
    end
  end

  defp phases(eligible, outside) do
    ph = eligible |> Enum.map(& &1.vblank_phase_us) |> Enum.reject(&is_nil/1)
    period = eligible |> Enum.map(& &1.refresh_period_us) |> Enum.reject(&is_nil/1)

    if ph != [] and period != [] do
      p = median(period)
      bins = 8
      hist =
        ph
        |> Enum.frequencies_by(&min(bins - 1, trunc(&1 / p * bins)))

      IO.puts("  present-end phase after vblank (period #{fmt(p / 1000, 2)} ms, #{bins} bins):")
      IO.puts("    " <>
                Enum.map_join(0..(bins - 1), " ", fn b ->
                  "#{fmt(b * p / bins / 1000, 1)}ms:#{Map.get(hist, b, 0)}"
                end))
      IO.puts("    phase p1 #{fmt(pct(ph, 0.01) / 1000, 2)} p50 #{fmt(pct(ph, 0.5) / 1000, 2)} " <>
                "p99 #{fmt(pct(ph, 0.99) / 1000, 2)} ms")
      op = outside |> Enum.map(& &1.vblank_phase_us) |> Enum.reject(&is_nil/1)
      if op != [] do
        IO.puts("    irregular pictures' phases (ms): " <>
                  (op |> Enum.take(20) |> Enum.map_join(" ", &fmt(&1 / 1000, 1))))
      end
    end
  end

  # ---- timing ----------------------------------------------------------

  defp intervals(rows) do
    ends = rows |> Enum.filter(&(&1.ready and not &1.blank)) |> Enum.map(& &1.emu_end_us)
    gaps = diffs(ends)
    pres = rows |> Enum.map(& &1.present_end_us) |> Enum.reject(&is_nil/1) |> diffs()
    emu = rows |> Enum.map(&(&1.emu_end_us - &1.emu_start_us))

    if gaps != [] do
      IO.puts("  picture interval ms: " <> spread(gaps))
    end

    if pres != [] do
      IO.puts("  present interval ms: " <> spread(pres))
    end

    IO.puts("  emulation per frame ms: " <> spread(emu))
  end

  defp wake(rows) do
    # One value per wake-up (several frames can share one).
    wakes =
      rows
      |> Enum.reject(&is_nil(&1.wake_late_us))
      |> Enum.uniq_by(& &1.wake_us)
      |> Enum.map(&{&1.wake_us, &1.wake_late_us})

    if wakes != [] do
      late = Enum.map(wakes, &elem(&1, 1))
      IO.puts("  wake late ms (#{length(late)} timed wake-ups): mean #{fmt(mean(late) / 1000, 3)} " <>
                spread(late))
      # Worst 10 s window by mean lateness (stretches of late wake-ups).
      windows =
        wakes
        |> Enum.group_by(fn {t, _} -> div(t, 10_000_000) end, &elem(&1, 1))
        |> Enum.filter(fn {_, v} -> length(v) >= 50 end)
        |> Enum.map(fn {w, v} -> {w, mean(v), Enum.max(v)} end)

      if windows != [] do
        {w, m, mx} = Enum.max_by(windows, &elem(&1, 1))
        {_, best, _} = Enum.min_by(windows, &elem(&1, 1))
        IO.puts("  wake late per 10 s window: worst mean #{fmt(m / 1000, 3)} ms (max #{fmt(mx / 1000, 2)}) " <>
                  "at #{w * 10}-#{w * 10 + 10} s, best mean #{fmt(best / 1000, 3)} ms")
      end
    end
  end

  defp marks(all) do
    ms = Enum.filter(all, & &1.mark)

    if ms != [] do
      IO.puts("\nmarks (frames within +-10 shown 0 or 2+ refreshes, or never presented):")
      by_frame = Map.new(all, &{&1.frame, &1})

      for m <- ms do
        near =
          for f <- (m.frame - 10)..(m.frame + 10),
              row <- [by_frame[f]],
              row != nil,
              row.ready and not row.blank,
              row.presents == 0 or (row.on_screen != nil and row.on_screen != 1),
              do: "f#{f}=" <> if(row.presents == 0, do: "unpresented", else: "#{row.on_screen}")

        IO.puts("  mark at frame #{m.frame} (#{fmt(m.emu_end_us / 1.0e6, 2)} s): " <>
                  if(near == [], do: "nothing irregular", else: Enum.join(near, " ")))
      end
    end
  end

  # ---- parsing ---------------------------------------------------------

  defp load(path) do
    lines = path |> File.read!() |> String.split("\n", trim: true)
    {comments, rest} = Enum.split_with(lines, &String.starts_with?(&1, "#"))
    [header | data] = rest
    cols = header |> String.split(",") |> Enum.map(&String.to_atom/1)
    rows = Enum.map(data, fn l -> parse_row(cols, String.split(l, ",")) end)
    {comments, rows}
  end

  @bools [:ready, :blank, :changed, :mark]
  @floats [:motion_match, :audio_fill_ms, :refresh_period_us, :vblank_phase_us]

  defp parse_row(cols, vals) do
    cols
    |> Enum.zip(vals)
    |> Map.new(fn
      {k, ""} -> {k, nil}
      {k, v} when k in @bools -> {k, v == "1"}
      {k, v} when k in @floats -> {k, elem(Float.parse(v), 0)}
      {k, v} -> {k, String.to_integer(v)}
    end)
    |> then(&Map.put(&1, :on_screen, &1[:on_screen_refreshes]))
  rescue
    e -> raise "bad row #{Enum.join(vals, ",")}: #{Exception.message(e)}"
  end

  defp header_rate(comments) do
    Enum.find_value(comments, fn c ->
      case Regex.run(~r/display refresh.*= ([0-9.]+) Hz/, c) do
        [_, hz] -> elem(Float.parse(hz), 0)
        _ -> nil
      end
    end)
  end

  defp rom(comments) do
    Enum.find_value(comments, "?", fn c ->
      case c do
        "# rom: " <> t -> t
        _ -> nil
      end
    end)
  end

  # ---- stats -----------------------------------------------------------

  defp diffs([]), do: []
  defp diffs(l), do: Enum.zip_with(tl(l), l, &(&1 - &2))
  defp mean(l), do: Enum.sum(l) / length(l)
  defp median(l), do: pct(l, 0.5)

  defp pct(l, q) do
    s = Enum.sort(l)
    n = length(s)
    i = max(1, min(n, ceil(q * n))) - 1
    Enum.at(s, i)
  end

  defp spread(l) do
    "p1 #{fmt(pct(l, 0.01) / 1000, 2)} p50 #{fmt(pct(l, 0.5) / 1000, 2)} " <>
      "p99 #{fmt(pct(l, 0.99) / 1000, 2)} max #{fmt(Enum.max(l) / 1000, 2)}"
  end

  defp fmt(x, d), do: :erlang.float_to_binary(x * 1.0, decimals: d)
end

FrameLog.main(System.argv())
