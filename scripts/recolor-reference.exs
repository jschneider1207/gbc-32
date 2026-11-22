# Recolors one RGB color in a pinned upstream reference PNG and writes the
# result as a committed reference override (acceptance/README.md,
# "Committed reference overrides"). Only for a human-approved correction.
#
#   elixir scripts/recolor-reference.exs <upstream> <out.png> <from> <to> <count>
#
#   <upstream>  path relative to test-roms/ (e.g. mbc3-tester/mbc3-tester-cgb.png)
#   <out.png>   where to write the result (e.g. acceptance/references/...)
#   <from>      RGB color to replace, hex (e.g. 7BFF4A)
#   <to>        replacement RGB color, hex (e.g. 7BFF31)
#   <count>     exact number of pixels that must change
#
# Refuses (exit 1, nothing written) unless:
#   - the upstream file's SHA-256 equals its line in acceptance/test-roms.sha256;
#   - exactly <count> pixels have color <from>, and they are the only pixels
#     that change (checked by decoding the written file again);
# Prints the SHA-256 of the written file, for the manifest's `sha256` pin.
#
# Handles 8-bit RGB and RGBA, non-interlaced PNGs with any filter types;
# writes the same color type with filter 0 and no ancillary chunks.
# No dependencies: Erlang's :zlib, :crypto and :erlang.crc32.

defmodule Png do
  @sig <<0x89, "PNG", 0x0D, 0x0A, 0x1A, 0x0A>>

  def decode(<<@sig, rest::binary>>) do
    chunks = chunks(rest, [])

    case chunks do
      [{"IHDR", ihdr} | _] -> decode_image(ihdr, chunks)
      _ -> fail("PNG does not start with IHDR")
    end
  end

  def decode(_), do: fail("not a PNG file")

  defp chunks(<<len::32, type::binary-4, data::binary-size(len), crc::32, rest::binary>>, acc) do
    if :erlang.crc32(type <> data) != crc, do: fail("bad CRC in #{type} chunk")
    acc = [{type, data} | acc]
    if type == "IEND", do: Enum.reverse(acc), else: chunks(rest, acc)
  end

  defp chunks(_, _), do: fail("truncated PNG (no IEND)")

  defp decode_image(<<w::32, h::32, depth, color, 0, 0, interlace>>, chunks) do
    if depth != 8, do: fail("bit depth #{depth} not supported (only 8)")
    if interlace != 0, do: fail("interlaced PNGs are not supported")

    bpp =
      case color do
        2 -> 3
        6 -> 4
        _ -> fail("color type #{color} not supported (only RGB and RGBA)")
      end

    idat = for {"IDAT", d} <- chunks, into: <<>>, do: d
    raw = :zlib.uncompress(idat)
    stride = w * bpp
    if byte_size(raw) != h * (stride + 1), do: fail("image data has the wrong size")

    {rows, _} =
      Enum.map_reduce(0..(h - 1), :binary.copy(<<0>>, stride), fn y, prev ->
        <<filter, line::binary-size(stride)>> = binary_part(raw, y * (stride + 1), stride + 1)
        row = unfilter(filter, line, prev, bpp)
        {row, row}
      end)

    %{width: w, height: h, color: color, bpp: bpp, data: IO.iodata_to_binary(rows)}
  end

  defp decode_image(_, _), do: fail("unsupported IHDR (compression/filter method)")

  # PNG spec section 9: reconstruct each byte from a (left), b (up), c (up-left).
  defp unfilter(0, line, _prev, _bpp), do: line

  defp unfilter(filter, line, prev, bpp) when filter in 1..4 do
    up = prev |> :binary.bin_to_list() |> List.to_tuple()

    line
    |> :binary.bin_to_list()
    |> Enum.with_index()
    # `acc` is the reconstructed row so far, reversed: the byte `bpp`
    # positions to the left is at index bpp - 1.
    |> Enum.reduce([], fn {x, i}, acc ->
      a = if i >= bpp, do: Enum.at(acc, bpp - 1), else: 0
      b = elem(up, i)
      c = if i >= bpp, do: elem(up, i - bpp), else: 0

      pred =
        case filter do
          1 -> a
          2 -> b
          3 -> div(a + b, 2)
          4 -> paeth(a, b, c)
        end

      [rem(x + pred, 256) | acc]
    end)
    |> Enum.reverse()
    |> :binary.list_to_bin()
  end

  defp unfilter(f, _, _, _), do: fail("unknown PNG filter type #{f}")

  defp paeth(a, b, c) do
    p = a + b - c
    pa = abs(p - a)
    pb = abs(p - b)
    pc = abs(p - c)

    cond do
      pa <= pb and pa <= pc -> a
      pb <= pc -> b
      true -> c
    end
  end

  def encode(%{width: w, height: h, color: color, bpp: bpp, data: data}) do
    stride = w * bpp
    raw = for y <- 0..(h - 1), into: <<>>, do: <<0>> <> binary_part(data, y * stride, stride)

    @sig <>
      chunk("IHDR", <<w::32, h::32, 8, color, 0, 0, 0>>) <>
      chunk("IDAT", :zlib.compress(raw)) <> chunk("IEND", <<>>)
  end

  defp chunk(type, data),
    do: <<byte_size(data)::32>> <> type <> data <> <<:erlang.crc32(type <> data)::32>>

  def pixels(%{bpp: bpp, data: data}), do: for(<<px::binary-size(bpp) <- data>>, do: px)

  def fail(msg) do
    IO.puts(:stderr, "recolor-reference: #{msg}")
    System.halt(1)
  end
end

defmodule Recolor do
  @root Path.expand("..", __DIR__)

  def main([src, out, from, to, count]) do
    from = rgb(from)
    to = rgb(to)
    if from == to, do: Png.fail("<from> and <to> are the same color")

    count =
      case Integer.parse(count) do
        {n, ""} when n > 0 -> n
        _ -> Png.fail("bad <count> #{inspect(count)}")
      end

    bytes = File.read!(Path.join([@root, "test-roms", src]))
    check_pinned!(src, bytes)
    img = Png.decode(bytes)
    rgb_of = fn px -> binary_part(px, 0, 3) end

    found = Enum.count(Png.pixels(img), &(rgb_of.(&1) == from))

    if found != count,
      do: Png.fail("#{found} pixels have color #{hex(from)}, expected exactly #{count}")

    new_data =
      for <<px::binary-size(img.bpp) <- img.data>>, into: <<>> do
        if rgb_of.(px) == from, do: to <> binary_part(px, 3, img.bpp - 3), else: px
      end

    encoded = Png.encode(%{img | data: new_data})

    # Decode what will be written and compare it with the upstream image
    # pixel by pixel: exactly `count` pixels change, all from -> to.
    back = Png.decode(encoded)

    if {back.width, back.height, back.bpp} != {img.width, img.height, img.bpp},
      do: Png.fail("re-decoded image has a different shape")

    diffs = Enum.zip(Png.pixels(img), Png.pixels(back)) |> Enum.reject(fn {a, b} -> a == b end)

    bad =
      Enum.reject(diffs, fn {a, b} ->
        rgb_of.(a) == from and rgb_of.(b) == to and
          binary_part(a, 3, img.bpp - 3) == binary_part(b, 3, img.bpp - 3)
      end)

    if bad != [] or length(diffs) != count,
      do: Png.fail("verification failed: #{length(diffs)} changed, #{length(bad)} unexpected")

    out_path = Path.expand(out)
    File.mkdir_p!(Path.dirname(out_path))
    File.write!(out_path, encoded)
    sha = :crypto.hash(:sha256, encoded) |> Base.encode16(case: :lower)

    IO.puts(
      "#{out}: #{count} pixels #{hex(from)} -> #{hex(to)}, #{img.width}x#{img.height}\nsha256 #{sha}"
    )
  end

  def main(_) do
    Png.fail("usage: elixir scripts/recolor-reference.exs <upstream> <out.png> <from> <to> <count>")
  end

  defp check_pinned!(src, bytes) do
    list = File.read!(Path.join([@root, "acceptance", "test-roms.sha256"]))

    want =
      list
      |> String.split("\n")
      |> Enum.find_value(fn line ->
        case String.split(line, "  ", parts: 2) do
          [h, ^src] -> h
          _ -> nil
        end
      end) || Png.fail("#{src} is not in acceptance/test-roms.sha256")

    got = :crypto.hash(:sha256, bytes) |> Base.encode16(case: :lower)

    if got != want,
      do: Png.fail("#{src} has SHA-256 #{got}, but acceptance/test-roms.sha256 pins #{want}")
  end

  defp rgb(s) do
    case Base.decode16(String.trim_leading(s, "#"), case: :mixed) do
      {:ok, <<_, _, _>> = c} -> c
      _ -> Png.fail("bad color #{inspect(s)} (want RRGGBB hex)")
    end
  end

  defp hex(c), do: "#" <> Base.encode16(c)
end

Recolor.main(System.argv())
