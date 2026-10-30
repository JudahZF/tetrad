import { type ReactNode, useState } from "react";
import { KeyboardAvoidingView, Modal, Platform, Pressable, ScrollView, StyleSheet, TextInput, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { Button, T } from "./kit";
import { usePalette } from "./theme";

/** A bottom sheet over the screen. */
export function Sheet({ visible, title, onClose, children }: { visible: boolean; title: string; onClose: () => void; children: ReactNode }) {
  const palette = usePalette();
  const insets = useSafeAreaInsets();
  return (
    <Modal visible={visible} transparent animationType="slide" onRequestClose={onClose}>
      <KeyboardAvoidingView behavior={Platform.OS === "ios" ? "padding" : undefined} style={{ flex: 1 }}>
        <Pressable style={styles.scrim} onPress={onClose} accessibilityLabel="Close" />
        <View style={[styles.sheet, { backgroundColor: palette.panel, paddingBottom: insets.bottom + 16 }]}>
          <View style={[styles.grab, { backgroundColor: palette.faint }]} />
          <T size={16} weight="bold" center>
            {title}
          </T>
          <ScrollView contentContainerStyle={{ gap: 10, paddingTop: 12 }} keyboardShouldPersistTaps="handled">
            {children}
          </ScrollView>
        </View>
      </KeyboardAvoidingView>
    </Modal>
  );
}

/** Asks for one line of text. */
export function TextPrompt(props: {
  visible: boolean;
  title: string;
  initial: string;
  hint?: string;
  action: string;
  onDone: (text: string) => void;
  onClose: () => void;
}) {
  return (
    <Sheet visible={props.visible} title={props.title} onClose={props.onClose}>
      {/* Keyed so that each opening starts from `initial`. */}
      {props.visible && <PromptBody key={props.initial} {...props} />}
    </Sheet>
  );
}

function PromptBody({ title, initial, hint, action, onDone }: { title: string; initial: string; hint?: string; action: string; onDone: (text: string) => void }) {
  const palette = usePalette();
  const [text, setText] = useState(initial);
  return (
    <>
      <TextInput
        value={text}
        onChangeText={setText}
        autoFocus
        onSubmitEditing={() => onDone(text)}
        style={{ color: palette.text, backgroundColor: palette.canvas, borderRadius: 8, paddingHorizontal: 12, height: 44, fontFamily: "IBMPlexSans_400Regular", fontSize: 15 }}
        accessibilityLabel={title}
      />
      {hint && (
        <T size={12} tone="muted">
          {hint}
        </T>
      )}
      <Button label={action} kind="accent" onPress={() => onDone(text)} />
    </>
  );
}

const styles = StyleSheet.create({
  scrim: { flex: 1, backgroundColor: "#000a" },
  sheet: { borderTopLeftRadius: 22, borderTopRightRadius: 22, paddingTop: 10, paddingHorizontal: 16, maxHeight: "85%" },
  grab: { width: 38, height: 5, borderRadius: 3, alignSelf: "center", marginBottom: 12 },
});
