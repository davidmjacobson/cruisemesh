# UniFFI-generated Rust bindings (core/build-android.sh, package
# uniffi.cruisemesh_core) bind native functions through JNA by name — R8
# must not rename or strip them, or the JNI hookup silently breaks.
-keep class uniffi.** { *; }
-keepclassmembers class uniffi.** { *; }

# JNA itself resolves native structs/callbacks via reflection.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure { *; }
-keepclassmembers class * implements com.sun.jna.Library { *; }
-dontwarn com.sun.jna.**

# Gson (de)serializes the relayd wire types in RelayClient by matching field
# names/@SerializedName reflectively; keep them stable under obfuscation.
-keepattributes Signature
-keepattributes *Annotation*
-keepclassmembers class com.cruisemesh.app.relay.** {
    <fields>;
}
