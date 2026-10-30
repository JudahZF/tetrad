/** The app's building blocks, drawn like the unit's cards, rows, tabs and switches. */

import type { ReactNode } from "react";
import {
  ActivityIndicator,
  Pressable,
  ScrollView,
  type StyleProp,
  StyleSheet,
  Text,
  type TextStyle,
  View,
  type ViewStyle,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Icon, type IconName } from "./Icon";
import { type Palette, font, usePalette } from "./theme";

type Tone = keyof Pick<Palette, "text" | "muted" | "faint" | "accent" | "danger" | "warning" | "onAccent">;

export function T({
  children,
  size = 14,
  weight = "regular",
  tone = "text",
  color,
  mono,
  center,
  lines,
  style,
}: {
  children: ReactNode;
  size?: number;
  weight?: "regular" | "medium" | "semibold" | "bold";
  tone?: Tone;
  color?: string;
  mono?: boolean;
  center?: boolean;
  lines?: number;
  style?: StyleProp<TextStyle>;
}) {
  const palette = usePalette();
  return (
    <Text
      numberOfLines={lines}
      style={[
        {
          color: color ?? palette[tone],
          fontSize: size,
          lineHeight: Math.round(size * 1.35),
          fontFamily: mono ? font.mono : font[weight],
        },
        center && { textAlign: "center" },
        style,
      ]}
    >
      {children}
    </Text>
  );
}

/** A tab screen: the safe area, a title row and scrolling content. */
export function Screen({
  title,
  right,
  children,
  scroll = true,
}: {
  title?: ReactNode;
  right?: ReactNode;
  children: ReactNode;
  scroll?: boolean;
}) {
  const palette = usePalette();
  const body = <View style={styles.body}>{children}</View>;
  return (
    <SafeAreaView edges={["top"]} style={{ flex: 1, backgroundColor: palette.background }}>
      {title !== undefined && (
        <View style={styles.appbar}>
          <View style={{ flex: 1 }}>
            {typeof title === "string" ? (
              <T size={22} weight="bold">
                {title}
              </T>
            ) : (
              title
            )}
          </View>
          {right}
        </View>
      )}
      {scroll ? (
        <ScrollView contentContainerStyle={{ paddingBottom: 32 }} keyboardShouldPersistTaps="handled">
          {body}
        </ScrollView>
      ) : (
        body
      )}
    </SafeAreaView>
  );
}

export function Card({ children, style }: { children: ReactNode; style?: StyleProp<ViewStyle> }) {
  const palette = usePalette();
  return <View style={[{ backgroundColor: palette.panel, borderRadius: 10, overflow: "hidden" }, style]}>{children}</View>;
}

/** Rows of a card, with the unit's 1 px divider between them. */
export function Rows({ children }: { children: ReactNode }) {
  const palette = usePalette();
  const items = (Array.isArray(children) ? children : [children]).flat().filter(Boolean);
  return (
    <Card>
      {items.map((child, i) => (
        <View key={i} style={i > 0 && { borderTopWidth: 1, borderTopColor: palette.divider }}>
          {child}
        </View>
      ))}
    </Card>
  );
}

export function Row({
  icon,
  lead,
  title,
  detail,
  right,
  chevron,
  onPress,
  disabled,
}: {
  icon?: IconName;
  lead?: ReactNode;
  title: ReactNode;
  detail?: ReactNode;
  right?: ReactNode;
  chevron?: boolean;
  onPress?: () => void;
  disabled?: boolean;
}) {
  const palette = usePalette();
  return (
    <Pressable
      onPress={onPress}
      disabled={disabled || !onPress}
      style={({ pressed }) => [styles.row, pressed && { backgroundColor: palette.rowSelected }, disabled && { opacity: 0.5 }]}
    >
      {icon && <IconBox name={icon} />}
      {lead}
      <View style={{ flex: 1, minWidth: 0 }}>
        {typeof title === "string" ? (
          <T size={14} weight="semibold" lines={1}>
            {title}
          </T>
        ) : (
          title
        )}
        {detail !== undefined &&
          (typeof detail === "string" ? (
            <T size={12} tone="muted" lines={2}>
              {detail}
            </T>
          ) : (
            detail
          ))}
      </View>
      {right}
      {chevron && <Icon name="forward" size={16} color={palette.faint} />}
    </Pressable>
  );
}

export function IconBox({ name, accent, size = 30 }: { name: IconName; accent?: boolean; size?: number }) {
  const palette = usePalette();
  return (
    <View style={[styles.iconBox, { width: size, height: size, backgroundColor: palette.canvas }]}>
      <Icon name={name} size={size * 0.64} color={accent ? palette.accent : palette.text} />
    </View>
  );
}

export function Section({ children }: { children: string }) {
  return (
    <T size={11} weight="semibold" tone="muted" style={styles.section}>
      {children.toUpperCase()}
    </T>
  );
}

/** Segmented tabs: the unit's selected tab is a dark green fill under an accent outline. */
export function Segmented<K extends string>({
  options,
  value,
  onChange,
}: {
  options: readonly { key: K; label: string }[];
  value: K;
  onChange: (key: K) => void;
}) {
  const palette = usePalette();
  return (
    <View style={[styles.seg, { backgroundColor: palette.surface }]}>
      {options.map((o) => {
        const on = o.key === value;
        return (
          <Pressable
            key={o.key}
            onPress={() => onChange(o.key)}
            style={[styles.segItem, on && { backgroundColor: palette.accentFill, borderColor: palette.accent }]}
            accessibilityRole="tab"
            accessibilityState={{ selected: on }}
          >
            <T size={12.5} weight="semibold" tone={on ? "accent" : "muted"} lines={1}>
              {o.label}
            </T>
          </Pressable>
        );
      })}
    </View>
  );
}

/** The unit's switch: a ring, and a knob that turns accent when on. */
export function Toggle({ value, onChange, disabled }: { value: boolean; onChange?: (v: boolean) => void; disabled?: boolean }) {
  const palette = usePalette();
  return (
    <Pressable
      onPress={() => onChange?.(!value)}
      disabled={disabled || !onChange}
      accessibilityRole="switch"
      accessibilityState={{ checked: value, disabled }}
      hitSlop={8}
      style={[
        styles.toggle,
        { backgroundColor: palette.canvas, borderColor: value ? palette.accent : palette.faint },
        disabled && { opacity: 0.5 },
      ]}
    >
      <View style={[styles.knob, { backgroundColor: value ? palette.accent : palette.faint, left: value ? 19 : 3 }]} />
    </Pressable>
  );
}

export function Button({
  label,
  icon,
  kind = "panel",
  onPress,
  disabled,
  busy,
  style,
  accessibilityLabel,
}: {
  label?: string;
  /** For a button with only an icon. */
  accessibilityLabel?: string;
  icon?: IconName;
  kind?: "panel" | "action" | "accent" | "ghost" | "danger";
  onPress?: () => void;
  disabled?: boolean;
  busy?: boolean;
  style?: StyleProp<ViewStyle>;
}) {
  const palette = usePalette();
  const background = {
    panel: palette.panel,
    action: palette.action,
    accent: palette.accent,
    ghost: palette.canvas,
    danger: palette.danger,
  }[kind];
  const ink = kind === "accent" ? palette.onAccent : palette.text;
  return (
    <Pressable
      onPress={onPress}
      disabled={disabled || busy}
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel ?? label ?? icon}
      style={({ pressed }) => [
        styles.button,
        { backgroundColor: background, opacity: disabled ? 0.45 : pressed ? 0.75 : 1 },
        style,
      ]}
    >
      {busy ? (
        <ActivityIndicator size="small" color={ink} />
      ) : (
        icon && <Icon name={icon} size={18} color={ink} />
      )}
      {label && (
        <T size={13} weight="semibold" color={ink}>
          {label}
        </T>
      )}
    </Pressable>
  );
}

export function Pill({ label, tone }: { label: string; tone: "ok" | "wait" | "bad" | "plain" }) {
  const palette = usePalette();
  const [color, background] = {
    ok: [palette.accent, palette.accentFill],
    wait: [palette.warning, "#2a2408"],
    bad: [palette.danger, "#2a0a0a"],
    plain: [palette.text, palette.section],
  }[tone];
  return (
    <View style={[styles.pill, { backgroundColor: background }]}>
      <T size={11} weight="semibold" color={color}>
        {label}
      </T>
    </View>
  );
}

/** A message that fills the space of a list: nothing yet, an error, or a feature this link lacks. */
export function Empty({ icon, title, detail, action }: { icon?: IconName; title: string; detail?: string; action?: ReactNode }) {
  const palette = usePalette();
  return (
    <View style={styles.empty}>
      {icon && <Icon name={icon} size={32} color={palette.faint} />}
      <T size={15} weight="semibold" center>
        {title}
      </T>
      {detail && (
        <T size={13} tone="muted" center>
          {detail}
        </T>
      )}
      {action}
    </View>
  );
}

export function Spinner() {
  const palette = usePalette();
  return <ActivityIndicator color={palette.accent} style={{ padding: 24 }} />;
}

export const styles = StyleSheet.create({
  body: { paddingHorizontal: 16, gap: 12 },
  appbar: { flexDirection: "row", alignItems: "center", gap: 10, minHeight: 48, paddingHorizontal: 16 },
  row: { flexDirection: "row", alignItems: "center", gap: 12, paddingVertical: 11, paddingHorizontal: 14, minHeight: 48 },
  iconBox: { borderRadius: 7, alignItems: "center", justifyContent: "center" },
  section: { letterSpacing: 0.9, marginTop: 4, marginHorizontal: 2, marginBottom: -4 },
  seg: { flexDirection: "row", borderRadius: 9, padding: 3, gap: 3 },
  segItem: { flex: 1, alignItems: "center", paddingVertical: 7, borderRadius: 7, borderWidth: 1, borderColor: "transparent" },
  toggle: { width: 40, height: 24, borderRadius: 12, borderWidth: 1.5 },
  knob: { position: "absolute", top: 2.5, width: 16, height: 16, borderRadius: 8 },
  button: { flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 6, borderRadius: 8, paddingVertical: 10, paddingHorizontal: 14, minHeight: 40 },
  pill: { borderRadius: 4, paddingHorizontal: 7, paddingVertical: 2 },
  empty: { alignItems: "center", gap: 8, paddingVertical: 32, paddingHorizontal: 24 },
});
