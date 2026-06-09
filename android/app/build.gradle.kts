import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    // AGP 9.0+ provides Kotlin built-in; the standalone `org.jetbrains.kotlin.android`
    // plugin must NOT be applied (doing so fails the build). Kotlin compiler options move to
    // the top-level `kotlin { compilerOptions { … } }` block below — the old
    // `android { kotlinOptions { … } }` DSL came from the now-removed plugin and is gone.
    id("com.android.application")
}

android {
    namespace = "com.rustscreen.client"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.rustscreen.client"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    ndkVersion = "25.2.9519653"

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // jniLibs already defaults to src/main/jniLibs (where cargo-ndk drops libandroid_client.so),
    // so no explicit sourceSets override is needed — the old `jniLibs.srcDirs(...)` call set the
    // default redundantly and is deprecated in AGP 9.

    // Release signing scaffold (P8). No keystore or password is committed — the
    // config reads them from the environment (preferred for CI) or from a personal,
    // git-ignored `keystore.properties` at the android/ root. If neither is present,
    // a release build is signed with the DEBUG key and a clear warning is printed,
    // so `./gradlew assembleRelease` still works locally for smoke-testing without
    // secrets. NEVER commit a real keystore or its passwords. See packaging/android.
    val keystorePropsFile = rootProject.file("keystore.properties")
    val keystoreProps = Properties().apply {
        if (keystorePropsFile.exists()) {
            keystorePropsFile.inputStream().use { load(it) }
        }
    }
    fun signingValue(envName: String, propName: String): String? =
        System.getenv(envName) ?: keystoreProps.getProperty(propName)

    val storeFilePath = signingValue("RUSTSCREEN_KEYSTORE", "storeFile")
    val hasReleaseSigning = storeFilePath != null

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = file(storeFilePath!!)
                storePassword = signingValue("RUSTSCREEN_KEYSTORE_PASSWORD", "storePassword")
                keyAlias = signingValue("RUSTSCREEN_KEY_ALIAS", "keyAlias")
                keyPassword = signingValue("RUSTSCREEN_KEY_PASSWORD", "keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (hasReleaseSigning) {
                signingConfigs.getByName("release")
            } else {
                // Fall back to the debug key so an unsigned-for-distribution release
                // APK can still be built locally. NOT suitable for distribution.
                logger.warn(
                    "RustScreen: no release keystore configured " +
                        "(set RUSTSCREEN_KEYSTORE env or android/keystore.properties); " +
                        "signing assembleRelease with the DEBUG key — do NOT distribute this APK."
                )
                signingConfigs.getByName("debug")
            }
        }
    }
}

// AGP 9's built-in Kotlin exposes the standard Kotlin Gradle DSL via the top-level `kotlin {}`
// extension (NOT `android { kotlinOptions { … } }`, which the removed plugin provided). Pin the
// Kotlin JVM bytecode target to 17 to match the Java `compileOptions` above — AGP fails the build
// on a Kotlin/Java target mismatch.
kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    // The thin shell (D7) uses only framework APIs (android.app.Activity, SurfaceView)
    // and loads the Rust cdylib via JNI — no AndroidX dependencies are required.
}
