import { View } from "react-native";
import { CATEGORIES, type Category } from "@/lib/preset/model";
import { Icon } from "./Icon";
import { mix, usePalette } from "./theme";

/**
 * A block as the Grid draws it: a black tile with a 2 px ring in the category colour and the
 * white glyph at half the tile. A bypassed block dims its ring and glyph.
 */
export function BlockTile({
  category,
  size = 34,
  bypassed,
  selected,
}: {
  category: Category;
  size?: number;
  bypassed?: boolean;
  selected?: boolean;
}) {
  const palette = usePalette();
  const { colour, icon } = CATEGORIES[category];
  return (
    <View
      style={{
        width: size,
        height: size,
        borderRadius: size * 0.16,
        borderWidth: 2,
        borderColor: bypassed ? mix(colour, "#000000", 0.3) : colour,
        backgroundColor: palette.canvas,
        alignItems: "center",
        justifyContent: "center",
        outlineColor: selected ? palette.text : undefined,
        outlineWidth: selected ? 2 : 0,
        outlineOffset: 2,
        outlineStyle: "solid",
      }}
    >
      <Icon name={icon} size={size * 0.56} color={bypassed ? "#ffffff40" : "#ffffff"} />
    </View>
  );
}
