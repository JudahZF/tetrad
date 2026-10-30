import { type ColorValue, Image, type ImageStyle, type StyleProp } from "react-native";
import { type IconName, iconAssets } from "./icon-assets";
import { usePalette } from "./theme";

export type { IconName };

/** A unit icon (tetrad-assets), white ink tinted to `color`. */
export function Icon({
  name,
  size = 20,
  color,
  style,
}: {
  name: IconName;
  size?: number;
  color?: ColorValue;
  style?: StyleProp<ImageStyle>;
}) {
  const palette = usePalette();
  return (
    <Image
      source={iconAssets[name]}
      style={[{ width: size, height: size, tintColor: color ?? palette.text }, style]}
      accessibilityIgnoresInvertColors
    />
  );
}
