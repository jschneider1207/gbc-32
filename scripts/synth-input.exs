# Writes an input recording (ADR 0030) from a short button program, so a
# run can be scripted without anyone playing it.
#
#   gbc-desktop <rom> --headless --frames 1 --record-input head.txt
#   elixir scripts/synth-input.exs head.txt program.txt out.txt
#
# head.txt gives the header (ROM checksums and hash, starting cartridge RAM
# and clock); make it from a scratch copy of the ROM with no .sav beside it
# for a fresh start. Its events are dropped. program.txt has one press per
# line:
#
#   <frame> <frames held> <button>[+<button>...]   e.g. `600 4 start`
#   end <frames>                                   total length
#
# Buttons: a b select start right left up down. `#` starts a comment.
# Presses may overlap; the held set is their union.
defmodule Synth do
  @bits %{
    "a" => 1,
    "b" => 2,
    "select" => 4,
    "start" => 8,
    "right" => 16,
    "left" => 32,
    "up" => 64,
    "down" => 128
  }

  def main([head, prog, out]) do
    header =
      head
      |> File.read!()
      |> String.split("\n")
      |> Enum.take_while(&(&1 != "events"))
      |> Enum.join("\n")

    {presses, total} =
      prog
      |> File.read!()
      |> String.split("\n")
      |> Enum.map(&(&1 |> String.split("#") |> hd() |> String.trim()))
      |> Enum.reject(&(&1 == ""))
      |> Enum.reduce({[], nil}, fn line, {ps, total} ->
        case String.split(line) do
          ["end", n] -> {ps, String.to_integer(n)}
          [f, d, b] -> {[{String.to_integer(f), String.to_integer(d), mask(b)} | ps], total}
          _ -> raise "bad line: #{line}"
        end
      end)

    total || raise "program needs an `end <frames>` line"

    held = fn f ->
      Enum.reduce(presses, 0, fn {s, d, m}, acc ->
        if f >= s and f < s + d, do: Bitwise.bor(acc, m), else: acc
      end)
    end

    # Only frames where a press starts or ends can change the held set.
    edges =
      presses
      |> Enum.flat_map(fn {s, d, _} -> [s, s + d] end)
      |> Enum.filter(&(&1 < total))
      |> Enum.uniq()
      |> Enum.sort()

    {events, _} =
      Enum.reduce(edges, {[], 0}, fn f, {evs, prev} ->
        b = held.(f)
        if b == prev, do: {evs, prev}, else: {["#{f} b #{hex(b)}" | evs], b}
      end)

    body = events |> Enum.reverse() |> Enum.map_join("", &(&1 <> "\n"))
    File.write!(out, header <> "\nevents\n" <> body <> "end #{total}\n")
    IO.puts("wrote #{out}: #{length(events)} changes, #{total} frames")
  end

  def main(_) do
    IO.puts(:stderr, "usage: elixir scripts/synth-input.exs head.txt program.txt out.txt")
    System.halt(2)
  end

  defp mask(spec) do
    spec
    |> String.split("+")
    |> Enum.reduce(0, fn n, acc -> Bitwise.bor(acc, Map.fetch!(@bits, String.downcase(n))) end)
  end

  defp hex(b), do: b |> Integer.to_string(16) |> String.downcase() |> String.pad_leading(2, "0")
end

Synth.main(System.argv())
