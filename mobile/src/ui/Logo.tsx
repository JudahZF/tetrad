import Svg, { ClipPath, Defs, G, Path, Rect } from "react-native-svg";

/** The Tetrad logo (docs/brand): a Grid block holding a pick cut into three faces. */
export function Logo({ size = 56, ring = "#45f862" }: { size?: number; ring?: string }) {
  return (
    <Svg width={size} height={size} viewBox="0 0 64 64" accessibilityLabel="Tetrad">
      <Rect x={2.5} y={2.5} width={59} height={59} rx={5.5} fill="#000" stroke={ring} strokeWidth={5} />
      <G transform="translate(7.593 8.041) scale(1.0169)">
        <Defs>
          <ClipPath id="pick">
            <Path d="M24 44.8C21.2 44.8 19.6 42.9 17.4 39.6C12 31.5 5 22 5 14.5C5 8 12 3.5 24 3.5C36 3.5 43 8 43 14.5C43 22 36 31.5 30.6 39.6C28.4 42.9 26.8 44.8 24 44.8Z" />
          </ClipPath>
        </Defs>
        <G clipPath="url(#pick)">
          <Path d="M28.5 19L-133.9-67.4-7.5 225.4Z" fill="#ff7000" />
          <Path d="M28.5 19L-133.9-67.4H118.9Z" fill="#ffd236" />
          <Path d="M28.5 19L118.9-67.4-7.5 225.4Z" fill="#6954ff" />
        </G>
      </G>
    </Svg>
  );
}
