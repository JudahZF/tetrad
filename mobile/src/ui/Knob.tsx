import { useState } from "react";
import { View } from "react-native";
import Svg, { Circle, Line, Path } from "react-native-svg";
import { type Param, clamp } from "@/lib/preset/model";
import { T } from "./kit";
import { usePalette } from "./theme";

const START = -225;
const SWEEP = 270;
/** Points of drag for the whole range. */
const TRAVEL = 160;

const rad = (deg: number) => (deg * Math.PI) / 180;

export function formatValue(param: Pick<Param, "step" | "unit">, value: number) {
  const digits = param.step >= 1 ? 0 : param.step >= 0.1 ? 1 : 2;
  const text = value.toFixed(digits);
  return param.unit ? `${text}${param.unit === "%" ? " %" : ` ${param.unit}`}` : text;
}

/**
 * A knob like the unit's: a track ring, an accent arc for the value and a pointer. A vertical
 * drag turns it; `onChange` runs while dragging, `onDone` when the finger lifts.
 */
export function Knob({
  param,
  value,
  size = 64,
  highlight,
  badge,
  onChange,
  onDone,
}: {
  param: Param;
  value: number;
  size?: number;
  highlight?: boolean;
  badge?: string;
  onChange: (value: number) => void;
  onDone?: (value: number) => void;
}) {
  const palette = usePalette();
  const [drag, setDrag] = useState<{ fromY: number; from: number; last: number } | null>(null);

  const move = (pageY: number) => {
    if (!drag) return;
    const next = clamp(param, drag.from - ((pageY - drag.fromY) / TRAVEL) * (param.max - param.min));
    if (next !== drag.last) {
      setDrag({ ...drag, last: next });
      onChange(next);
    }
  };
  const end = () => {
    if (drag) onDone?.(drag.last);
    setDrag(null);
  };

  const share = (value - param.min) / (param.max - param.min || 1);
  const angle = START + share * SWEEP;
  const r = 24;
  const point = (deg: number, radius = r) => [32 + radius * Math.cos(rad(deg)), 32 + radius * Math.sin(rad(deg))] as const;
  const [x0, y0] = point(START);
  const [x1, y1] = point(angle);
  const [xe, ye] = point(START + SWEEP);
  const [kx, ky] = point(angle, 15);

  return (
    <View
      onStartShouldSetResponder={() => true}
      onMoveShouldSetResponder={() => true}
      onResponderTerminationRequest={() => false}
      onResponderGrant={(e) => setDrag({ fromY: e.nativeEvent.pageY, from: value, last: value })}
      onResponderMove={(e) => move(e.nativeEvent.pageY)}
      onResponderRelease={end}
      onResponderTerminate={end}
      style={{ alignItems: "center", gap: 2 }}
      accessibilityRole="adjustable"
      accessibilityLabel={param.name}
      accessibilityValue={{ text: formatValue(param, value) }}
      accessibilityActions={[{ name: "increment" }, { name: "decrement" }]}
      onAccessibilityAction={(e) => {
        const step = (param.max - param.min) / 20;
        const next = clamp(param, value + (e.nativeEvent.actionName === "increment" ? step : -step));
        onChange(next);
        onDone?.(next);
      }}
    >
      <Svg width={size} height={size} viewBox="0 0 64 64">
        <Path d={`M${x0} ${y0}A${r} ${r} 0 1 1 ${xe} ${ye}`} stroke={palette.rowSelected} strokeWidth={5} fill="none" strokeLinecap="round" />
        {share > 0.001 && (
          <Path
            d={`M${x0} ${y0}A${r} ${r} 0 ${share * SWEEP > 180 ? 1 : 0} 1 ${x1} ${y1}`}
            stroke={palette.accent}
            strokeWidth={5}
            fill="none"
            strokeLinecap="round"
          />
        )}
        <Circle cx={32} cy={32} r={18} fill={palette.canvas} />
        <Line x1={32} y1={32} x2={kx} y2={ky} stroke={palette.text} strokeWidth={2.5} strokeLinecap="round" />
      </Svg>
      {badge && (
        <View style={{ position: "absolute", top: 0, right: 2, backgroundColor: palette.warning, borderRadius: 3, paddingHorizontal: 3 }}>
          <T size={9} weight="bold" color="#000">
            {badge}
          </T>
        </View>
      )}
      <T size={12} mono tone={highlight ? "accent" : "text"}>
        {formatValue(param, value)}
      </T>
      <T size={11} tone="muted" lines={1}>
        {param.name}
      </T>
    </View>
  );
}
