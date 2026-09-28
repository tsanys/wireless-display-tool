plugins {
    id("com.android.application")
}

android {
    namespace = "com.wdt.receiver"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.wdt.receiver"
        minSdk = 21
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    // Eksplisit agar tidak tergantung default source dir AGP.
    sourceSets.named("main") {
        kotlin.directories += "src/main/kotlin"
    }
    sourceSets.named("test") {
        kotlin.directories += "src/test/kotlin"
    }
}

// DSL pengganti android.kotlinOptions (lihat developer.android.com/build/migrate-to-built-in-kotlin).
kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    // Prebuilt libwebrtc maintained (org.webrtc:google-webrtc mati sejak 2018).
    // Versi 150.7871.01 = latest di Maven Central per 2026-09-26 (masih terbaru T5).
    implementation("io.github.webrtc-sdk:android:150.7871.01")

    // WebSocket signaling client.
    // Pin 5.3.2: versi 5.x terbaru yang masih kompatibel compileSdk 34
    // (okhttp-android 5.4.0+ menuntut compileSdk 36/37).
    implementation("com.squareup.okhttp3:okhttp:5.5.0")
    // JSON parsing kontrak signaling (tanpa annotation processor/plugin).
    implementation("com.google.code.gson:gson:2.14.0")
    // Coroutines untuk NSD/WS/WebRTC callback off-main-thread.
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")

    // Unit test kontrak protokol (JVM, tanpa device).
    testImplementation("junit:junit:4.13.2")
}
